import {normalizeHudStyle} from './view-selection.js';

export function hudStyleToIndex(style) {
    return normalizeHudStyle(style) === 'wave' ? 1 : 0;
}

export function hudStyleFromIndex(index) {
    return index === 1 ? 'wave' : 'basic';
}

export function connectHudStyle(settings, onStyle) {
    let active = true;
    const apply = () => {
        if (active)
            onStyle(settings.get_string('hud-style'));
    };
    const signalId = settings.connect('changed::hud-style', apply);
    apply();
    return () => {
        if (!active)
            return;
        active = false;
        settings.disconnect(signalId);
    };
}
