# IE115 — Session lifecycle (state + event diagrams)

**Date:** 2026-07-01
**Status:** Draft (action item from the 2026-07-01 sync — Charles + Farshid)
**Authors:** Claude, with Charles

The 2026-07-01 sync agreed IE115 is a *suitable subset* of the OpenAI Realtime
API, extended with **additive events** (unknown events are ignored by unaware
clients). This note draws the session lifecycle, with the parts Farshid flagged
as the hard bit: it is **asynchronous** — model residency, audio flow, and
finalization are not a single clean sequence.

## Event legend (aliases used below)

Client → Server:

| alias  | IE115 event                 | notes                                                          |
|--------|-----------------------------|----------------------------------------------------------------|
| UPDATE | `session.update`            | config patch/merge                                             |
| APPEND | `input_audio_buffer.append` | PCM; base64-in-JSON now, raw binary frame later (Jun-24 hatch) |
| COMMIT | `input_audio_buffer.commit` | end of utterance (hotkey release)                              |

Server → Client:

| alias   | IE115 event                                              | notes                                           |
|---------|----------------------------------------------------------|-------------------------------------------------|
| CREATED | `session.created`                                        | server defaults                                 |
| UPDATED | `session.updated`                                        | effective config                                |
| STATUS  | *liveness event* `{state: loading\|ready\|transcribing}` | **ADDITIVE, agreed 2026-07-01; name TBD**       |
| DELTA   | `conversation.item.input_audio_transcription.delta`      | incremental text                                |
| DONE    | `conversation.item.input_audio_transcription.completed`  | full text for a committed utterance             |
| ERROR   | `error {type, code, message}`                            | terminal *or* recoverable (taxonomy still open) |

> **STATUS** is the model-loading/liveness signal agreed on 2026-07-01
> ("model-ready is always an event"; loading is a lifecycle state, **not** an
> error). Naming and exact payload are for this diagram pass + the spec edit to
> settle. OpenAI has no equivalent (cloud is always warm) — it is a deliberate
> local extension.

---

## 1. Happy path — push-to-talk, with async model load

Note the two independent tracks: **session negotiation** and **model
residency**. `STATUS{loading}` / `STATUS{ready}` can arrive before *or* after
`UPDATED` — the client must gate audio on `STATUS{ready}`, not on `UPDATED`.

```
 Client                                        Server
   |                                             |
   |---------------- WS connect ---------------->|
   |<--------------- CREATED (defaults) ---------|   session bootstrap
   |                                             |
   |---------------- UPDATE (config) ----------->|
   |<--------------- UPDATED (effective) --------|   } negotiation and model load
   |                                             |   } are concurrent — no fixed
   |<--------------- STATUS{loading} ------------|   } order between UPDATED and
   |     (client: show "warming up",             |   } STATUS{...}
   |      DO NOT send audio yet)                  |
   |                                             |
   |<--------------- STATUS{ready} --------------|   model resident: safe to talk
   |                                             |
   |=== user holds hotkey, speaks ===            |
   |---------------- APPEND (pcm) -------------->|
   |---------------- APPEND (pcm) -------------->|
   |<--------------- STATUS{transcribing} -------|   liveness (optional)
   |<--------------- DELTA "the quick" ----------|
   |---------------- APPEND (pcm) -------------->|
   |<--------------- DELTA "brown fox" ----------|
   |                                             |
   |=== user releases hotkey ===                 |
   |---------------- COMMIT --------------------->|  end of utterance
   |                                             |  (server keeps decoding —
   |<--------------- DELTA "jumps." -------------|   tail deltas after COMMIT
   |<--------------- DONE (full transcript) -----|   are expected; see §3C)
   |                                             |
   |   next utterance? loop APPEND.. / COMMIT    |
   |   or:                                        |
   |---------------- WS close ------------------->|
   |                                             |
```

---

## 2. Server-side state — session FSM + orthogonal residency region

Two concurrent state regions per connection. Audio is only *processed* when the
**accept-gate** below is satisfied; otherwise it is dropped (§3A).

```
  +===================== SESSION (per connection) =======================+
  |                                                                      |
  |  [*] --connect--> CREATED --UPDATE--> ( validate )                   |
  |                                          |                           |
  |                        invalid --> ERROR |  valid                    |
  |                        (recoverable:     v                           |
  |                         client re-UPDATEs)                           |
  |                                       UPDATED ----> ACTIVE           |
  |                                                        |             |
  |                          APPEND (gate open) --------->  | (streaming)|
  |                          DELTA emitted <-------------  |             |
  |                                                        |             |
  |                                    COMMIT              |             |
  |                                       v                              |
  |                                  FINALIZING (drain + decode tail)    |
  |                                       |                              |
  |                                     DONE                             |
  |                                       v                              |
  |                                   ACTIVE  (ready for next utterance) |
  |                                       |                              |
  |                                   WS close                           |
  |                                       v                              |
  |                                      [*]                             |
  |                                                                      |
  |   ERROR may interpose from any state:                                |
  |     recoverable -> stay/return to ACTIVE (client may retry)          |
  |     terminal    -> connection closes                                 |
  +======================================================================+

  +================ MODEL RESIDENCY (async, orthogonal) =================+
  |                                                                      |
  |   UNLOADED --(trigger: connect / first use / preload)--> LOADING     |
  |      ^                                             |  emits STATUS{loading}
  |      |                                             v                  |
  |      |                                          RESIDENT  emits STATUS{ready}
  |      |                                             |                  |
  |      +-------------- idle timeout (N min) ---------+                  |
  |                                                    |                  |
  |                          load failed --> ERROR(model_load_failed)     |
  |                                          (terminal)                   |
  +======================================================================+

  ACCEPT-GATE  (when is an APPEND actually processed?)
      processed   <=>   SESSION == ACTIVE   AND   RESIDENCY == RESIDENT
      otherwise   ->    dropped   (see §3A)
```

Why two regions: the backend often starts before the user acts, and runtimes may
idle-unload the model between utterances. So residency changes **independently**
of the session — `RESIDENT` can lapse back to `LOADING` mid-connection, and the
next `APPEND` must re-gate on `STATUS{ready}`.

---

## 3. The async edge cases

### 3A. Audio arrives before the model is ready

Agreed: the client should wait for `STATUS{ready}`; audio sent early is the
client's fault and is **dropped** (no buffering server-side).

```
 Client                         Server
   |---- APPEND (pcm) --------->|  gate CLOSED (RESIDENCY != RESIDENT)
   |                          [dropped]
   |<--- ERROR(not_ready) ------|  advisory (see OPEN below) — non-terminal
   |<--- STATUS{ready} ---------|
   |---- APPEND (pcm) --------->|  gate OPEN -> processed
```

> **OPEN decision:** drop *silently*, or drop **and** emit an advisory
> `ERROR(not_ready)`? The sync leaned "client fault, we should signal something
> is wrong." Recommend: drop + one non-terminal advisory error so a
> misbehaving client can self-correct and we can log the contract violation —
> but it must **not** be treated as terminal (loading itself is not an error).

### 3B. Error mid-stream

```
 Client                         Server
   |---- APPEND (pcm) --------->|
   |<--- DELTA ---------------- |
   |---- APPEND (pcm) --------->|  internal failure (decode / OOM / model dropped)
   |<--- ERROR(code) ----------|
   |                            |  terminal  -> server closes
   |                            |  recoverable -> return to ACTIVE, client retries
```

> Depends on the **error taxonomy** (still open, plan T31): each `code` must
> declare terminal-vs-recoverable and client-vs-server fault so the client knows
> whether to retry or surface a hard failure. `model_load_failed` (3B/OOM) is
> terminal; a transient decode hiccup may be recoverable.

### 3C. Buffer drain on COMMIT — COMMIT is not "done"

The single most important async subtlety: releasing the hotkey stops *input*, it
does not mean transcription is finished. The client must keep the socket open
and wait for `DONE`.

```
 Client                         Server
   |---- APPEND (pcm) --------->|  buffered / still decoding
   |=== release hotkey ===      |
   |---- COMMIT --------------->|  no more audio — but tail not yet decoded
   |                            |  FINALIZING: drain buffer, finish decode
   |<--- DELTA (tail) ----------|
   |<--- DELTA (tail) ----------|
   |<--- DONE (full text) ------|  <-- only now is the utterance complete
   |---- WS close ------------->|  client closes AFTER DONE (never on COMMIT)
```

---

## Open items this diagram surfaces (for the spec edit)

1. **STATUS event name + payload** — settle the additive liveness event
   (`state` enum: `loading` / `ready` / `transcribing`; anything else?).
2. **Pre-ready audio** (§3A) — silent drop vs advisory `ERROR(not_ready)`.
3. **Error taxonomy** (§3B) — terminal/recoverable + fault side per code (T31).
4. **Residency re-lapse** — confirm the client must re-gate on `STATUS` if the
   model idle-unloads between utterances on a persistent connection.
5. **Overload/lag signal** — Matias's "falling behind / dropped chunk" event is
   another additive server event; where does it sit in this FSM? (out of scope
   for this draft, flagged for the next pass).
```
