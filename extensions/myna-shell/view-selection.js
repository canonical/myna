export function normalizeHudStyle(style) {
    return style === 'wave' ? 'wave' : 'basic';
}

export function createSelectedView(style, options, constructors) {
    return constructors[normalizeHudStyle(style)](options);
}
