# IM Extension Protocol v1

This document defines the JSON messages exchanged by Myna and an input-method
extension provider. The machine-readable definition is
`protocol-v1.schema.json`.

## Transport

- Myna connects to a same-user Unix-domain socket named by the active provider.
- Each utterance uses one connection, from acquire through release.
- Each JSON object is one newline-terminated UTF-8 line.
- A line is at most 1 MiB including its newline.
- Myna requires the socket owner and connected peer to be the current user.
- The provider sends no message before receiving `hello`.

## Handshake

Myna sends:

```json
{"type":"hello","version":1}
```

The provider answers:

```json
{
  "type": "ready",
  "version": 1,
  "capabilities": {
    "preedit": true,
    "surrounding_text": true,
    "secure_field_detection": true
  }
}
```

`secure_field_detection` must be true. A provider that cannot identify password
or sensitive fields is unavailable and cannot be activated. `preedit` and
`surrounding_text` may be false.

## Utterance

Myna sends `acquire`, then one or more `activate`, `commit`, or `preedit`
messages, and finally one `release`. An `activate` may arrive before
`acquire`; the provider must retain it for the utterance being established.

The provider answers `acquire` with:

```json
{"type":"acquired","before_cursor":"世"}
```

`before_cursor` is null or absent when unknown. When present, it must contain
exactly one Unicode character.

`commit` and `preedit` carry a monotonically increasing request number and
text:

```json
{"type":"commit","request":1,"text":"你好"}
{"type":"preedit","request":2,"text":"你"}
```

The provider acknowledges each with:

```json
{"type":"ack","request":1}
```

`preedit` replaces the preceding preedit text. An empty string clears it.
`commit` inserts stable text once and clears preedit. The provider must never
insert the preedit through `commit`.

`activate` has no response:

```json
{"type":"activate"}
```

It tells the provider that the user's activation is still current, including
key repeats while a shortcut is held. The provider uses it to distinguish a
focus blip caused by a key grab from a real focus loss.

`release` carries a request number and is acknowledged after preedit is
cleared:

```json
{"type":"release","request":3}
```

## Asynchronous events

The provider may send:

- `{"type":"focus_lost"}` when the acquired target loses focus.
- `{"type":"target_gone"}` when its window or input context disappears.
- `{"type":"secure"}` when the target becomes a password or sensitive field.

After `focus_lost`, `target_gone`, or `secure`, the provider must reject any
further text or preedit for that connection. It must clear preedit on `secure`.

An error is:

```json
{"type":"error","code":"no_target","message":"no editable target"}
```

Error codes are `no_target`, `secure_target`, `focus_lost`, `unavailable`, and
`protocol_error`. The message is diagnostic text and must not contain
transcript content.

## Lifecycle rules

- One connection owns one target. A provider cannot retarget an active
  connection.
- The provider must serialize requests and acknowledgements. A response to a
  request retains that request number.
- Myna serializes all writes, including asynchronous activation notifications.
- A malformed, oversized, unsupported, or disconnected stream ends the
  utterance with an explicit error.
- Unicode `commit` text is literal. The provider must not interpret it as
  key input or pass it through another input engine for composition.
- Transcript text must not appear in logs, diagnostics, or error messages.
