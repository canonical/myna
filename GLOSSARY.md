# Myna Text Input

Shared language for how Myna commits dictated text through its text-input boundary and system input method frameworks.

## Input method concepts

**Input method framework**:
A system component that owns input contexts and routes text input for applications, such as Fcitx5 or IBus.
_Avoid_: input method, IME, input backend

**Input method engine**:
A language-specific component that converts typing or composition inside an input method framework, such as Pinyin, Rime, Mozc, or Hangul.
_Avoid_: input method framework, input method

## Extension concepts

**Text-input boundary**:
The framework-neutral boundary through which Myna binds a target and commits dictated text.
_Avoid_: injection layer, input method

**Text-input backend**:
An implementation of Myna's `Injector` contract, such as the built-in IBus backend or an extension provider.
_Avoid_: input method, input framework

**Injector**:
The Myna component that acquires the input target for an utterance.
_Avoid_: provider, input method backend

**Target**:
The focused input context into which Myna may commit one utterance.
_Avoid_: text field, application

**Input-method extension (IM extension)**:
Independently packaged software outside the Myna project that adds Myna text-input compatibility for one input method framework.
_Avoid_: plugin, addon, input method engine

**IM extension protocol**:
The open, framework-neutral protocol through which an IM extension provides Myna text-input compatibility.
_Avoid_: Fcitx support, plugin API, third-party input support

**Input-method provider**:
The runtime endpoint supplied by an IM extension that binds Myna to a target through its input method framework.
_Avoid_: extension, input backend

**Provider manifest**:
Data installed by an IM extension package that declares the provider's identity, protocol versions, capabilities, and fixed socket name.
_Avoid_: plugin executable, launcher

**Active provider**:
The one provider a user has explicitly selected for Myna to use.
_Avoid_: default provider, preferred socket
