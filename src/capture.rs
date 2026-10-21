//! Microphone capture through PipeWire, delivered as 16 kHz mono `f32`.
//!
//! The stream asks PipeWire for exactly that format, so the session's adapter
//! does the resampling and downmix. The PipeWire main loop runs on its own
//! thread and stops when the receiver is dropped.

use std::sync::mpsc;
use std::thread;

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::pod::{Object, Pod, Value, serialize::PodSerializer};

use crate::sound::SAMPLE_RATE;

/// Starts capturing from `target` (a node name or serial, as `wpctl status`
/// or `pw-cli ls Node` show it), or from the default source. Each message is
/// one buffer of samples as PipeWire delivered it.
pub fn start(target: Option<String>) -> Result<mpsc::Receiver<Vec<f32>>, String> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    thread::Builder::new()
        .name("pipewire-capture".into())
        .spawn(move || {
            if let Err(e) = run(target, tx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })
        .map_err(|e| e.to_string())?;
    ready_rx
        .recv()
        .map_err(|_| "capture thread ended before starting".to_owned())??;
    Ok(rx)
}

fn run(
    target: Option<String>,
    tx: mpsc::Sender<Vec<f32>>,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    fn err(what: &'static str) -> impl Fn(pw::Error) -> String {
        move |e| format!("PipeWire: {what}: {e}")
    }
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(err("main loop"))?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(err("context"))?;
    let core = context.connect_rc(None).map_err(err("connect"))?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::NODE_NAME => "omatuber-sound",
    };
    if let Some(target) = target {
        props.insert(*pw::keys::TARGET_OBJECT, target);
    }
    let stream =
        pw::stream::StreamBox::new(&core, "omatuber-sound", props).map_err(err("stream"))?;

    let quit = mainloop.clone();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(|_, _, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(param).is_ok()
                && (info.format() != AudioFormat::F32LE
                    || info.rate() != SAMPLE_RATE as u32
                    || info.channels() != 1)
            {
                eprintln!(
                    "PipeWire negotiated {:?} {} Hz × {}, not F32LE 16000 Hz × 1",
                    info.format(),
                    info.rate(),
                    info.channels()
                );
            }
        })
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let (offset, size) = (data.chunk().offset() as usize, data.chunk().size() as usize);
            let Some(bytes) = data.data() else {
                return;
            };
            let Some(bytes) = bytes.get(offset..offset + size) else {
                return;
            };
            let samples = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            if tx.send(samples).is_err() {
                quit.quit();
            }
        })
        .register()
        .map_err(err("listener"))?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(SAMPLE_RATE as u32);
    info.set_channels(1);
    let format = Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(format))
        .map_err(|e| format!("PipeWire: format: {e:?}"))?
        .0
        .into_inner();
    let mut params = [Pod::from_bytes(&bytes).ok_or("PipeWire: invalid format pod")?];
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(err("stream connect"))?;

    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}
