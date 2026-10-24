//! Optional Linux GPU output. All EGL objects stay on one worker thread.
//! Pixels are uploaded when an image frame changes, composed on the GPU and
//! exported as immutable DMA-BUFs; there is no framebuffer readback.
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fs::{self, File};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::{
    avatar, layout,
    mouth::Mouth,
    server::{Images, Outputs, Scene, Served},
};

unsafe extern "C" {
    fn omt_gpu_create(node: *const c_char, error: *mut c_char, size: usize) -> *mut c_void;
    fn omt_gpu_destroy(gpu: *mut c_void);
    fn omt_upload(rgba: *const u8, width: u32, height: u32) -> u32;
    fn omt_delete_texture(texture: u32);
    fn omt_begin(gpu: *mut c_void, width: u32, height: u32) -> c_int;
    fn omt_draw(gpu: *mut c_void, texture: u32, x: f32, y: f32, w: f32, h: f32);
    fn omt_publish(gpu: *mut c_void, socket: c_int) -> c_int;
}

pub(crate) struct Snapshot {
    pub mouth: Mouth,
    pub blinking: bool,
    pub images: Images,
    pub scene: Scene,
    pub assets: HashMap<String, Served>,
}

/// Opt in explicitly; normal builds and the browser backend remain usable.
pub fn enabled() -> bool {
    std::env::var_os("OMATUBER_DMABUF").is_some_and(|v| v == "1")
}

pub struct Handle {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    /// Runtime failures are visible to the app, not silently replaced by CPU output.
    pub status: tokio::sync::watch::Receiver<Result<(), String>>,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn start(outputs: Outputs) -> Result<Handle, String> {
    let runtime =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or("XDG_RUNTIME_DIR is required for DMA-BUF")?;
    let node =
        std::env::var_os("OMATUBER_DRM_NODE").unwrap_or_else(|| "/dev/dri/renderD128".into());
    let node = CString::new(node.as_encoded_bytes()).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let (ready, rx) = mpsc::sync_channel(1);
    let (status_tx, status) = tokio::sync::watch::channel(Ok(()));
    let worker = std::thread::Builder::new()
        .name("omatuber-dmabuf".into())
        .spawn(move || {
            let result: Result<(), String> = (|| {
                let socket = Listener::bind(&PathBuf::from(runtime).join("omatuber"))?;
                let mut renderer = Renderer::new(&node)?;
                let _ = ready.send(Ok(()));
                while !flag.load(Ordering::Relaxed) {
                    let Some(client) = socket.accept()? else {
                        continue;
                    };
                    while !flag.load(Ordering::Relaxed) {
                        if !wait_readable(client.as_raw_fd(), 100)? {
                            continue;
                        }
                        let mut request = [0u8; 5];
                        // The extra byte detects overlong seqpacket requests.
                        let n = unsafe {
                            libc::recv(
                                client.as_raw_fd(),
                                request.as_mut_ptr().cast(),
                                request.len(),
                                libc::MSG_DONTWAIT,
                            )
                        };
                        if n != 4 || &request[..4] != b"NEXT" {
                            break;
                        }
                        if !renderer.render(outputs.dmabuf_snapshot(), client.as_raw_fd())? {
                            break;
                        }
                    }
                }
                Ok(())
            })();
            if let Err(e) = &result {
                let _ = ready.send(Err(e.clone()));
                eprintln!("OmaTuber DMA-BUF: {e}");
            }
            let _ = status_tx.send_replace(result);
        })
        .map_err(|e| e.to_string())?;
    match rx.recv().map_err(|e| e.to_string())? {
        Ok(()) => Ok(Handle {
            stop,
            worker: Some(worker),
            status,
        }),
        Err(e) => {
            let _ = worker.join();
            Err(e)
        }
    }
}

fn wait_readable(fd: c_int, timeout: c_int) -> Result<bool, String> {
    let mut p = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut p, 1, timeout) };
    if result < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(e.to_string());
    }
    Ok(result > 0)
}

struct Listener {
    fd: OwnedFd,
    path: PathBuf,
    _lock: File,
}
impl Listener {
    fn bind(dir: &Path) -> Result<Self, String> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .map_err(|e| e.to_string())?;
        let meta = fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(
                "DMA-BUF runtime directory must be owned by this user with mode 0700".into(),
            );
        }
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(dir.join("producer.lock"))
            .map_err(|e| e.to_string())?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("another OmaTuber DMA-BUF producer is already running".into());
        }
        let path = dir.join("frames.sock");
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        let raw = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        if raw < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as _;
        let bytes = path.as_os_str().as_encoded_bytes();
        if bytes.len() >= address.sun_path.len() {
            return Err("DMA-BUF socket path too long".into());
        }
        for (dest, &src) in address.sun_path.iter_mut().zip(bytes) {
            *dest = src as _;
        }
        let bound = unsafe {
            libc::bind(
                raw,
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of_val(&address) as _,
            )
        };
        if bound != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let listener = Self {
            fd,
            path,
            _lock: lock,
        };
        fs::set_permissions(&listener.path, fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        if unsafe { libc::listen(raw, 1) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(listener)
    }
    fn accept(&self) -> Result<Option<OwnedFd>, String> {
        if !wait_readable(self.fd.as_raw_fd(), 100)? {
            return Ok(None);
        }
        let raw = unsafe {
            libc::accept4(
                self.fd.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            )
        };
        if raw < 0 {
            return Ok(None);
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut size = std::mem::size_of_val(&peer) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut peer as *mut libc::ucred).cast(),
                &mut size,
            )
        } != 0
            || peer.uid != unsafe { libc::geteuid() }
        {
            return Ok(None);
        }
        Ok(Some(fd))
    }
}
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
impl Drop for Listener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

struct Cached {
    bytes: Arc<[u8]>,
    image: avatar::Png,
    texture: u32,
    frame: Option<usize>,
    started: f64,
    trigger: u64,
}
impl Drop for Cached {
    fn drop(&mut self) {
        unsafe {
            omt_delete_texture(self.texture);
        }
    }
}
struct Renderer {
    gpu: *mut c_void,
    cache: HashMap<String, Cached>,
    start: Instant,
    bounce: layout::Bounce,
}
impl Drop for Renderer {
    fn drop(&mut self) {
        self.cache.clear();
        unsafe {
            omt_gpu_destroy(self.gpu);
        }
    }
}
impl Renderer {
    fn new(node: &CStr) -> Result<Self, String> {
        let mut error = [0 as c_char; 256];
        let gpu = unsafe { omt_gpu_create(node.as_ptr(), error.as_mut_ptr(), error.len()) };
        if gpu.is_null() {
            return Err(unsafe { CStr::from_ptr(error.as_ptr()) }
                .to_string_lossy()
                .into_owned());
        }
        Ok(Self {
            gpu,
            cache: HashMap::new(),
            start: Instant::now(),
            bounce: layout::Bounce::default(),
        })
    }
    fn render(&mut self, snapshot: Snapshot, socket: c_int) -> Result<bool, String> {
        let now = self.start.elapsed().as_secs_f64();
        let open = snapshot.mouth == Mouth::Open;
        self.bounce.mouth(open, now);
        let images = &snapshot.images;
        let canvas = layout::canvas(images.sizes[0], images.sizes[1]).unwrap_or(layout::Canvas {
            width: 512,
            height: 512,
        });
        let bounce = images.bounce.clamp(0.0, layout::BOUNCE_MAX);
        let total_height = canvas.height + (canvas.height as f32 * bounce).ceil() as u32;
        let (width, height) = crate::scene::source_size(canvas.width, total_height);
        let (scale, origin) =
            layout::fit_with_headroom(canvas, [width as f32, height as f32], bounce);
        let lift = self.bounce.height(bounce, now).0 * canvas.height as f32 * scale;
        let origin = [origin[0], origin[1] - lift];
        let mut available = snapshot.assets;
        for (key, value) in [
            ("base-closed", &images.closed),
            ("base-open", &images.open),
            ("blink-closed", &images.blink[0]),
            ("blink-open", &images.blink[1]),
        ] {
            if let Some(image) = value {
                available.insert(key.into(), image.clone());
            }
        }
        let layers = layers(&snapshot.scene, open, snapshot.blinking, &available);
        // Keep base images across mouth/blink switches; retain other decoded
        // images only while they belong to the active scene.
        let mut keep: HashSet<&str> =
            ["base-closed", "base-open", "blink-closed", "blink-open"].into();
        if let Some(s) = &snapshot.scene.sentiment {
            keep.extend(s.quiet.as_deref());
            keep.extend(s.talking.as_deref());
        }
        keep.extend(snapshot.scene.objects.iter().map(|o| o.asset.as_str()));
        self.cache.retain(|key, cached| {
            keep.contains(key.as_str())
                && available
                    .get(key)
                    .is_some_and(|v| Arc::ptr_eq(&cached.bytes, &v.0))
        });
        if unsafe { omt_begin(self.gpu, width, height) } != 0 {
            return Err("cannot allocate/render linear ABGR8888 DMA-BUF".into());
        }
        for layer in layers {
            let Some((bytes, _)) = available.get(&layer.key) else {
                continue;
            };
            if !self.cache.contains_key(&layer.key) {
                let image = avatar::decode(bytes.to_vec(), avatar::Limits::default())
                    .map_err(|e| e.to_string())?;
                let retained: usize = self
                    .cache
                    .values()
                    .map(|c| c.image.frames.iter().map(|f| f.rgba.len()).sum::<usize>())
                    .sum();
                let decoded: usize = image.frames.iter().map(|f| f.rgba.len()).sum();
                if retained + decoded > 512 * 1024 * 1024 {
                    return Err("DMA-BUF decoded image cache exceeds 512 MiB".into());
                }
                self.cache.insert(
                    layer.key.clone(),
                    Cached {
                        bytes: bytes.clone(),
                        image,
                        texture: 0,
                        frame: None,
                        started: now,
                        trigger: layer.trigger,
                    },
                );
            }
            let cached = self.cache.get_mut(&layer.key).unwrap();
            if cached.trigger != layer.trigger {
                cached.started = now;
                cached.trigger = layer.trigger;
            }
            let index = cached
                .image
                .frame_at(Duration::from_secs_f64(now - cached.started));
            if cached.frame != Some(index) {
                let mut rgba = cached.image.frames[index].rgba.clone();
                for pixel in rgba.as_chunks_mut::<4>().0 {
                    let a = u16::from(pixel[3]);
                    for c in &mut pixel[..3] {
                        *c = ((u16::from(*c) * a + 127) / 255) as u8;
                    }
                }
                let texture =
                    unsafe { omt_upload(rgba.as_ptr(), cached.image.width, cached.image.height) };
                if texture == 0 {
                    return Err("cannot upload avatar texture".into());
                }
                unsafe {
                    omt_delete_texture(cached.texture);
                }
                cached.texture = texture;
                cached.frame = Some(index);
            }
            let p = placement(&layer, &cached.image, canvas, images.offset);
            unsafe {
                omt_draw(
                    self.gpu,
                    cached.texture,
                    origin[0] + p.x * scale,
                    origin[1] + p.y * scale,
                    p.width * scale,
                    p.height * scale,
                );
            }
        }
        Ok(unsafe { omt_publish(self.gpu, socket) } == 0)
    }
}

struct Layer {
    key: String,
    trigger: u64,
    kind: Placement,
}
enum Placement {
    Base(bool),
    Contained,
    Object { x: f32, y: f32, scale: f32 },
}
fn layers(
    scene: &Scene,
    open: bool,
    blink: bool,
    available: &HashMap<String, Served>,
) -> Vec<Layer> {
    let sentiment = scene.sentiment.as_ref().and_then(|s| {
        let key = if open {
            s.talking
                .as_ref()
                .filter(|k| available.contains_key(*k))
                .or(s.quiet.as_ref())
        } else {
            s.quiet.as_ref()
        }?;
        available.contains_key(key).then(|| Layer {
            key: key.clone(),
            trigger: s.trigger,
            kind: Placement::Contained,
        })
    });
    let base_open = open && available.contains_key("base-open");
    let base = if base_open {
        "base-open"
    } else {
        "base-closed"
    };
    let blinking = if base_open {
        "blink-open"
    } else {
        "blink-closed"
    };
    let key = if blink && available.contains_key(blinking) {
        blinking
    } else {
        base
    };
    let mut layers = vec![sentiment.unwrap_or(Layer {
        key: key.into(),
        trigger: 0,
        kind: Placement::Base(base_open),
    })];
    layers.extend(scene.objects.iter().map(|o| Layer {
        key: o.asset.clone(),
        trigger: scene.rev,
        kind: Placement::Object {
            x: o.x,
            y: o.y,
            scale: o.scale,
        },
    }));
    layers
}
fn placement(
    layer: &Layer,
    image: &avatar::Png,
    canvas: layout::Canvas,
    offset: layout::Offset,
) -> layout::Placed {
    let size = [image.width, image.height];
    match layer.kind {
        Placement::Base(open) => layout::place(canvas, size, if open { offset } else { [0, 0] }),
        Placement::Contained => {
            let scale = (canvas.width as f32 / image.width as f32)
                .min(canvas.height as f32 / image.height as f32);
            let width = image.width as f32 * scale;
            let height = image.height as f32 * scale;
            layout::Placed {
                x: (canvas.width as f32 - width) / 2.0,
                y: (canvas.height as f32 - height) / 2.0,
                width,
                height,
            }
        }
        Placement::Object { x, y, scale } => {
            let width = canvas.width as f32 * scale;
            let height = width * image.height as f32 / image.width as f32;
            layout::Placed {
                x: x * canvas.width as f32 - width / 2.0,
                y: y * canvas.height as f32 - height / 2.0,
                width,
                height,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{SceneObject, SceneSentiment};
    fn assets(keys: &[&str]) -> HashMap<String, Served> {
        keys.iter()
            .map(|k| ((*k).into(), (Arc::from(&b"image"[..]), "image/png")))
            .collect()
    }
    #[test]
    fn missing_render_node_is_a_reported_error() {
        let node = CString::new("/nonexistent/omatuber-drm-render-node").unwrap();
        match Renderer::new(&node) {
            Err(error) => assert!(error.contains("cannot open DRM render node")),
            Ok(_) => panic!("a missing DRM node cannot initialize the exporter"),
        }
    }

    #[test]
    fn layer_fallback_blink_and_object_order_match_browser() {
        let mut scene = Scene {
            sentiment: Some(SceneSentiment {
                quiet: Some("quiet".into()),
                talking: Some("talking".into()),
                trigger: 7,
            }),
            objects: vec![SceneObject {
                asset: "cup".into(),
                x: 0.2,
                y: 0.8,
                scale: 0.3,
            }],
            ..Default::default()
        };
        let available = assets(&["base-closed", "base-open", "blink-open", "quiet", "cup"]);
        let shown = layers(&scene, true, true, &available);
        assert_eq!(
            shown.iter().map(|l| l.key.as_str()).collect::<Vec<_>>(),
            ["quiet", "cup"]
        );
        assert_eq!(shown[0].trigger, 7);
        scene.sentiment = None;
        assert_eq!(layers(&scene, true, true, &available)[0].key, "blink-open");
        assert_eq!(
            layers(&scene, false, true, &available)[0].key,
            "base-closed"
        );
    }
    #[test]
    fn private_socket_has_single_producer_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("omatuber-ipc-{}", uuid::Uuid::new_v4()));
        let listener = Listener::bind(&dir).unwrap();
        assert_eq!(
            fs::metadata(dir.join("frames.sock")).unwrap().mode() & 0o777,
            0o600
        );
        assert!(Listener::bind(&dir).is_err());
        assert!(
            dir.join("frames.sock").exists(),
            "second producer must not unlink live socket"
        );
        drop(listener);
        assert!(!dir.join("frames.sock").exists());
        let restarted = Listener::bind(&dir).unwrap();
        drop(restarted);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn refuses_world_accessible_runtime_directory() {
        let dir = std::env::temp_dir().join(format!("omatuber-ipc-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Listener::bind(&dir).is_err());
        fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn contained_and_object_placement_preserve_aspect_and_anchor() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbaImage::new(40, 20)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let image = avatar::decode(png.into_inner(), avatar::Limits::default()).unwrap();
        let canvas = layout::Canvas {
            width: 100,
            height: 100,
        };
        let layer = Layer {
            key: "x".into(),
            trigger: 0,
            kind: Placement::Contained,
        };
        assert_eq!(
            placement(&layer, &image, canvas, [0, 0]),
            layout::Placed {
                x: 0.,
                y: 25.,
                width: 100.,
                height: 50.
            }
        );
        let layer = Layer {
            kind: Placement::Object {
                x: 0.5,
                y: 0.5,
                scale: 0.4,
            },
            ..layer
        };
        assert_eq!(
            placement(&layer, &image, canvas, [0, 0]),
            layout::Placed {
                x: 30.,
                y: 40.,
                width: 40.,
                height: 20.
            }
        );
    }
}
