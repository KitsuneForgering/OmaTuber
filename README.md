# OmaTuber

OmaTuber is a Linux desktop app for using an illustrated avatar in OBS. It
switches between quiet and talking images based on the volume of an audio
input selected in OBS. You can add blinking, a bounce when speaking, expressions,
and objects over the avatar.

The app is written in Rust with an egui interface. The interface is currently
in Portuguese.

## How it works

OmaTuber connects to OBS through its local WebSocket server and reads the
selected input's volume levels. A configurable threshold opens the avatar's
mouth; a lower closing threshold and a short hold keep it from flickering
between images during speech. The app combines the images and active reactions
into a transparent avatar source for OBS.

There are two output backends:

- DMA-BUF: the installed launcher uses this experimental backend by default.
  OmaTuber composes the avatar with EGL/GLES2 and sends GPU buffer descriptors
  to its OBS plugin over a local Unix socket. GPU compatibility and rendering
  still need validation in a real OBS session.
- Browser: OBS loads the avatar from a local page at
  `http://127.0.0.1:39017/`. The page receives state changes from OmaTuber and
  displays the images. Start with `OMATUBER_DMABUF=0 omatuber` to use it.

Optional sound reactions use PipeWire to listen for sounds you teach the app,
such as your laugh. They use a locally trained classifier and trigger an
expression you choose. Mouth movement uses OBS volume levels independently
of this feature.

## Build and install

The complete build needs Linux, Rust, a C compiler, make, pkg-config, PipeWire
development files, OBS >= 32 development files, EGL, GLES2, GBM, and libdrm.
DMA-BUF also needs a compatible GPU and access to its DRM render node.
See the [DMA-BUF guide](docs/dmabuf.md) for backend requirements and limitations.

From the project root:

```sh
make
omatuber
```

`make` builds and installs the app and OBS plugin for the current user, with
DMA-BUF enabled in the launcher. Restart OBS after installation. The launcher
is installed in `~/.local/bin`, which must be in your `PATH`. This installation
targets native OBS; Flatpak OBS has not been validated.

`make build` only builds; `make run` builds and runs the local app with DMA-BUF;
`make test` runs the native and Rust tests.

## First use

1. Enable the WebSocket server in OBS and check its port and password. OmaTuber
   connects to OBS on the same computer, using port `4455` by default.
2. Open OmaTuber. Choose a PNG for `Quieto` (quiet) and another for `Falando`
   (talking), or use `Usar exemplo` to try the included avatar.
3. Enter the OBS password, connect, and select the audio input that should
   control the mouth. Speak and adjust `Sensibilidade`, or use
   `Calibrar sensibilidade` to collect silence and speech levels.
4. Use `Adicionar à cena` to add the avatar to a selected scene, or
   `Configurar todas as cenas` to share it across the current scene collection.
5. Check the result in OBS and keep OmaTuber open while using the avatar.

Without OBS connected, `Testar boca` lets you check the talking image in the
preview. This test only affects the preview.

## Expressions and objects

In `Reações e objetos`, a sentiment is a named expression with quiet and
optional talking images. You choose how long it stays on screen, its priority,
and any objects that appear with it. Objects are image layers, such as glasses
or a mug, with adjustable position and size. Use the draft preview to test expressions and drag objects into place.
Choose `Aplicar ao OBS` to save and apply library edits.

Expressions can be triggered manually, by a trained sound, or after a chosen
period of silence. You can also call the running app from a desktop shortcut:

```sh
omatuber reagir "Animado"
omatuber base
```

Replace `Animado` with the name of an enabled expression in your library.
`base` returns to the base avatar.

## Documentation

- [Avatar setup, sound reactions, and saved data](docs/usage.md)
- [OBS scene collection setup (Portuguese)](docs/obs-scenes.md)
- [Experimental DMA-BUF backend (Portuguese)](docs/dmabuf.md)

OmaTuber is available under the [MIT license](LICENSE).
