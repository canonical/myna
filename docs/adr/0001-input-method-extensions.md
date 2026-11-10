---
status: accepted
---

# Use out-of-tree input-method extensions for semantic text commit

Myna needs to commit dictated text through input method frameworks other than IBus. The Linux desktop has no cross-desktop semantic text-commit interface: X11 key injection is layout- and input-method-sensitive, and Wayland deliberately prevents arbitrary clients from injecting input into other applications. RemoteDesktop/libei exposes device events rather than text commits, surrounding text, secure-field state, or preedit. Myna will therefore keep the text-input boundary framework-neutral, but expose a versioned provider protocol that out-of-tree input-method extensions (IM extensions) implement inside their input method framework. Myna core will not contain Fcitx5 or another provider's framework-specific code.

The one active provider is explicitly selected by the user. Myna uses IBus only when no provider is active. If an active provider disappears or fails, the current utterance fails visibly instead of silently switching to IBus. Provider packages use system package metadata and PackageKit for installation; Myna does not operate a package marketplace or download arbitrary code.

## Considered Options

- **RemoteDesktop/libei key emulation** would work across compositors that implement the portal, but it emulates keyboard devices. It cannot provide semantic commits, surrounding text, secure-field detection, or replacement-safe preedit, and it can interact unsafely with the user's current layout and input method.
- **AT-SPI text manipulation** depends on applications exposing suitable accessibility objects and is racy and inconsistent as a general text-injection mechanism.
- **Framework-specific code in Myna** would make each new input framework a change to the Myna repository. It would increase review, packaging, and maintenance cost and make one framework too central to the design.
- **A session-bus D-Bus interface** would be more familiar and introspectable than a private local protocol, but it adds bus-name, policy, and service-lifecycle surface to the confined snap. Myna already uses private Unix sockets where confinement makes that boundary simpler. The message semantics can move to another transport later if maintainers prefer it.

## Consequences

Myna owns a protocol, its versioning policy, its security boundary, and conformance tests. Extension authors own the input-method-framework integration and its package. This creates a small but real ongoing protocol-maintenance burden and a new local attack surface that must require same-user peer credentials, bounded messages, explicit enablement, and strict validation.

The design initially targets Fcitx5 as a real provider. A Wayland `zwp_input_method_v2` provider is a neutrality probe rather than a second product backend. Input method engines such as Pinyin, Rime, Mozc, and Hangul are supported through their input method framework and do not add Myna backends.

## Non-normative protocol sketch

This sketch records the current design direction only; it is not a stable wire contract.

- Each utterance uses a private Unix-domain socket connection owned by the provider.
- Messages are newline-delimited JSON with a versioned hello and bounded payload sizes.
- One writer serializes that connection's requests and asynchronous activation notifications.
- Myna sends acquire, activation, commit, preedit, and release requests.
- The provider reports acquired target capabilities, asynchronous focus loss, secure-field changes, acknowledgements, and classified errors.
- Myna Settings reads installed extension manifests and stores the explicitly selected provider id in the shared settings store. The confined daemon reads only that id and derives the fixed provider socket under its runtime directory; it does not read host system manifests.
- Secure-field detection is required before a provider can be activated. A target that becomes secure ends the utterance immediately and clears any preedit.
- The common protocol covers the complete `Target` contract; framework-specific behavior stays inside the provider.
