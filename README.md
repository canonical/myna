# myna
Speech to text on Ubuntu

Myna is a lightweight speech-to-text application for Ubuntu Desktop.

The project draws its name from the [myna](https://en.wikipedia.org/wiki/Myna), a bird renowned for its ability to listen to, mimic, and reproduce human speech with astonishing clarity. Just like its avian counterpart, this application is designed to master voice audio-listening intently to your spoken words and instantly translating them into accurate, clean text. Whether you are looking to dictate text hands-free, improve accessibility, or streamline your workflow, myna brings seamless voice recognition directly to your Linux ecosystem.

## Repository layout

- `src/myna/core` — shared vocabulary: audio types, transcript events, session config, transport abstraction
- `src/myna/testbed` — candidate-adapter evaluation testbed (fake adapter, harness, metrics)
- `src/myna/desktop` — interface stubs for the Ubuntu Desktop dictation client (UD129)
- `docs/architecture` — architecture decision records; read before structural changes
- `docs/project-plan.md` — workstreams, tasks, and milestones
- `reference/` — local checkouts of related projects (inference snaps, CLI); not committed

## Development

Tooling is [uv](https://docs.astral.sh/uv/):

```shell
uv sync                                  # install
uv run pytest                            # contract tests
uv run python -m myna.testbed            # Phase 0 demo: fake adapter via loopback
uv run python dev/generate_fixtures.py   # synthesize the fixture corpus into fixtures/
                                         # (needs libespeak-ng1 + espeak-ng-data)
```
