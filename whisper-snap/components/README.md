# Model component weights

Downloaded CTranslate2 Whisper weights live here, one directory per model, as
the source for the snap's model components (see `snap/snapcraft.yaml`
`model-components` part). They are **not** committed — populate them before
packing:

```shell
../dev/download-models.sh          # tiny base small
```

Each `model-<size>-ct2/` directory holds a faster-whisper CTranslate2
conversion of `Systran/faster-whisper-<size>` (MIT): `model.bin`,
`config.json`, `tokenizer.json`, `vocabulary.txt`, `preprocessor_config.json`.
At pack time they are routed into the `model-<size>` snap components.
