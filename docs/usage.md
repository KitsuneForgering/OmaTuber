# Using OmaTuber

Start with the [README](../README.md) for installation and the first connection
to OBS. The interface labels below match the app's Portuguese interface.

## Prepare the avatar

Use a quiet image and a talking image, preferably with transparent backgrounds.
The app accepts PNG files up to 16 MiB and 4096 pixels per side. If the talking
image does not line up with the quiet image, adjust its horizontal and vertical
offsets under `Alinhar a imagem Falando`.

`Pulo ao falar` adds a bounce when the mouth opens. Under `Piscar (opcional)`,
you can add eyes-closed versions of the quiet and talking images. Each blink
image must have the same dimensions as its corresponding base image.

The image decoder supports PNG, APNG, and GIF. Animated base images are
experimental and require the `OMATUBER_ANIMATED` environment variable to be set.
Start with still PNGs while checking your setup.

## Adjust mouth movement

Select the audio input in OBS that should drive the avatar. OmaTuber receives
its volume levels through OBS WebSocket and uses them to decide whether the
mouth is open. It does not need to capture microphone samples for this step.

Adjust `Sensibilidade` while speaking at your normal volume. For calibration,
choose `Calibrar sensibilidade`, stay quiet for the first collection, then
speak for the second. Review the result and choose `Aplicar sugestão` to use it.
The calibration collects levels only.

If the mouth does not react, check the connection status, selected input, and
whether that input is muted in OBS. Without fresh volume readings, OmaTuber
closes the mouth.

## Add the output to OBS

`Adicionar à cena` creates or reuses a source for the active backend in the
selected scene. `Configurar todas as cenas` configures the current collection
with a shared source. It preserves existing placement and visibility where
supported and hides old avatar instances for recovery. It reports scenes
that need manual attention. See the [scene setup guide](obs-scenes.md) for
nested scenes, filters, and migration limitations.

The installed launcher selects DMA-BUF. If the source has no image, check the
error shown in OmaTuber and use `Atualizar diagnóstico` in the OBS source's
properties. The [DMA-BUF guide](dmabuf.md) explains the diagnostic states.
To use the browser backend, launch with `OMATUBER_DMABUF=0 omatuber` and add
or configure the source from the app again.

The DMA-BUF plugin provides an OBS source. It does not create a virtual camera
for browsers or calling apps.

## Set up reactions

In `Reações e objetos`, add a sentiment and choose its quiet image and optional
talking image. Without a talking image, that expression keeps its quiet image
while you speak. Set a duration or choose `Fica até o próximo` to keep it until
another expression or an explicit return to base.

Higher-priority expressions take precedence over an expression already on
screen; equal priority goes to the newest trigger. An expression can also
appear after a chosen period of silence. The base avatar returns when you
speak again.

Select an expression or object to edit one item at a time. The `Prévia do
rascunho` shows your edits locally; switch between `Quieto` and `Falando`
without affecting OBS. Duration, priority, silence, and sound triggers are
under `Duração e gatilhos`.

Add objects as separate image layers. Select an object and drag it in the
preview to position it, or use the numeric controls for precise placement and
size. Position is the object's center as a percentage of the avatar canvas;
size is a percentage of its width. Select which objects appear with each
expression.

Click `Aplicar ao OBS` to save and apply the library. `Mostrar no OBS` activates
the applied item; apply pending changes to that item first. `Descartar
alterações` restores the entire draft to the applied library. Base avatar
settings on the `Avatar` page are saved and sent to OBS automatically; the
library preview remains local.

For shortcuts, use `omatuber reagir "Name"` with an enabled expression's name,
or `omatuber base` to return to the base avatar. These commands contact the
running app through its local HTTP server on port `39017`.

## Teach sound reactions

Under `Reagir a sons`, open `Ensinar sons` and record examples of the sounds
you want to recognize. Training needs at least two sound classes and two clips
per class. Include `fala` for ordinary speech and `outros` for background
noises to help avoid false triggers. Review the training report before using
the model.

Assign a trained sound to an expression under `Som que aciona`, apply the
library, then enable `Ouvir o microfone`. This feature captures the default
PipeWire audio source; `OMATUBER_SOURCE` can select a different node by name or
serial. This source is separate from the OBS input used for mouth movement.

OmaTuber analyzes captured audio locally with a naive Bayes classifier.
Live detection processes samples in memory and discards them. Clips explicitly
recorded for training are saved as WAV files. A detected class triggers the
expression you assigned to that sound; the expression names are your choices.

## Saved data

Preferences and the reaction library are stored in
`$XDG_CONFIG_HOME/omatuber/`, or `~/.config/omatuber/` when that variable is
unset:

- `prefs.json`: OBS port, selected input, base image paths, and avatar settings.
- `library.json`: expressions, object image paths, and reaction settings.

Keep the referenced image files available when reopening the app. These JSON
files store paths to your images.

Sound training data is stored in `$XDG_DATA_HOME/omatuber/`, falling back to
`~/.local/share/omatuber/`. The `sounds/` directory contains recorded examples;
`bayes.json` contains the trained model. Set `OMATUBER_DATA` to override this
data directory.

If you enable `Lembrar senha neste computador`, the OBS password is stored
through the desktop's Secret Service keyring. It is kept out of the preference
files and source URL.
