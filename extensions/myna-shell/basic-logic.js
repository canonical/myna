import {FLOOR, levelsToIntensity} from './vumeter.js';

export const BASIC_ATTACK_TAU_MS = 45;
export const BASIC_RELEASE_TAU_MS = 120;

function clamp01(value) {
    if (!Number.isFinite(value))
        return 0;
    return Math.max(0, Math.min(1, value));
}

export function basicTargetFill(stateKey, rms, peak, ageMs = 0) {
    if (stateKey !== 'recording')
        return 0;
    const intensity = levelsToIntensity(rms, peak, ageMs);
    return clamp01((intensity - FLOOR) / (1 - FLOOR));
}

export function smoothBasicFill(previous, target, dtMs, reducedMotion = false) {
    const next = clamp01(target);
    if (reducedMotion || dtMs <= 0)
        return next;
    const current = clamp01(previous);
    const tau = next > current ? BASIC_ATTACK_TAU_MS : BASIC_RELEASE_TAU_MS;
    const alpha = 1 - Math.exp(-dtMs / tau);
    return clamp01(current + (next - current) * alpha);
}
