# Model component checkpoint

The FastConformer `.nemo` checkpoint lives here as the source for the
`model-streaming-multi` component. Not committed — populate before packing:

```shell
../dev/download-models.sh
```

This fetches `nvidia/stt_en_fastconformer_hybrid_large_streaming_multi` (the
`.nemo` file) into `model-streaming-multi/`. At pack time it's routed into the
`model-streaming-multi` snap component; the adapter restores it directly
(`ASRModel.restore_from`), so no network at runtime.
