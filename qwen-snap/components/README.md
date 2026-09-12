# Model component weights

Downloaded Qwen3-ASR weights live here, one directory per model, as the source
for the snap's model components (see `snap/snapcraft.yaml` `model-components`
part). They are **not** committed — populate them before packing:

```shell
../dev/download-models.sh          # 0.6B and 1.7B
```

Each `Qwen3-ASR-<size>/` directory holds the Hugging Face model (Apache-2.0):
`model.safetensors`, `config.json`, `tokenizer_config.json`, `vocab.json`,
`merges.txt`, `preprocessor_config.json`. At pack time they are routed into the
`model-<size>` snap components, loaded by both runtimes from the directory (no
network access).
