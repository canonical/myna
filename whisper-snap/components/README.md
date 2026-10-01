# Model component weights

Downloaded CTranslate2 Whisper weights live here, one directory per model, as
the source for the snap's model components (see `snap/snapcraft.yaml`
`model-components` part). They are **not** committed — populate them before
packing:

```shell
../dev/download-models.sh          # tiny base small large-v3 large-v3-turbo
```

Each `model-<size>-ct2/` directory holds a faster-whisper CTranslate2
conversion of `Systran/faster-whisper-<size>` (MIT): `model.bin`,
`config.json`, `tokenizer.json`, `vocabulary.txt`, `preprocessor_config.json`.
The exception is `model-large-v3-turbo-ct2/`, which converts
`dropbox-dash/faster-whisper-large-v3-turbo` (MIT) - Systran never published
a turbo conversion.
At pack time they are routed into the `model-<size>` snap components.
