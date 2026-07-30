/*
 * Pure placement arithmetic for the Stickies shell extension.
 *
 * Deliberately free of `gi://` and gnome-shell imports so it can be exercised
 * by `gjs -m extension/test.js` outside a live session. Everything that needs
 * Meta, Clutter or Main lives in extension.js.
 *
 * This mirrors src/model/geometry.rs on the app side. Both clamp, because
 * neither trusts the other: the app works from a monitor list that may be
 * seconds old, and the extension may be handed values from an older app build.
 */

/**
 * Clamp a monitor-relative rectangle so it sits fully inside a work area, and
 * convert it to absolute compositor coordinates.
 *
 * @param {{x: number, y: number, width: number, height: number}} area
 *   Work area in absolute coordinates.
 * @param {number} x Monitor-relative left edge.
 * @param {number} y Monitor-relative top edge.
 * @param {number} width Desired width.
 * @param {number} height Desired height.
 * @returns {{x: number, y: number, width: number, height: number}} Absolute.
 */
export function clampToWorkArea(area, x, y, width, height) {
    // Size first: clamping a position against a size that does not fit would
    // produce an inverted range and put the window off-screen.
    const w = Math.max(1, Math.min(Math.round(width), area.width));
    const h = Math.max(1, Math.min(Math.round(height), area.height));

    return {
        x: area.x + clamp(Math.round(x), 0, area.width - w),
        y: area.y + clamp(Math.round(y), 0, area.height - h),
        width: w,
        height: h,
    };
}

/**
 * Convert an absolute window rectangle into coordinates relative to its
 * monitor's work area — the form the app persists.
 *
 * @param {{x: number, y: number}} area Work area in absolute coordinates.
 * @param {{x: number, y: number, width: number, height: number}} rect
 * @returns {[number, number, number, number]} `[x, y, width, height]`
 */
export function toRelative(area, rect) {
    return [rect.x - area.x, rect.y - area.y, rect.width, rect.height];
}

function clamp(value, low, high) {
    // `high` can fall below `low` only if the caller skipped the size clamp;
    // biasing to `low` keeps the window on screen rather than off its left edge.
    if (high < low)
        return low;
    return Math.max(low, Math.min(value, high));
}

/**
 * Coerce a monitor into the exact `(ssbiiii)` tuple `ListMonitors` declares,
 * or `null` if it cannot be represented.
 *
 * GJS packs a D-Bus reply by walking the declared signature and converting each
 * JS value; anything that is not the expected primitive — a null connector, an
 * undefined work area, a float where an int is declared — fails deep inside
 * `_packVariant` with "Service implementation returned an incorrect value type"
 * and no indication of which field or which monitor was at fault.
 *
 * So the boundary coerces explicitly rather than trusting the compositor's
 * shape, and drops entries it cannot make sense of instead of failing the whole
 * call: one unusable monitor should cost that monitor, not every monitor.
 *
 * @param {string|null} connector
 * @param {string|null} displayName
 * @param {boolean} isPrimary
 * @param {{x: number, y: number, width: number, height: number}|null} area
 * @returns {[string, string, boolean, number, number, number, number]|null}
 */
export function monitorEntry(connector, displayName, isPrimary, area) {
    if (typeof connector !== 'string' || connector === '')
        return null;
    if (!area || !Number.isFinite(area.width) || !Number.isFinite(area.height))
        return null;

    const int = value => Math.round(Number(value) || 0);
    const width = int(area.width);
    const height = int(area.height);
    if (width <= 0 || height <= 0)
        return null;

    return [
        connector,
        typeof displayName === 'string' && displayName !== '' ? displayName : connector,
        Boolean(isPrimary),
        int(area.x),
        int(area.y),
        width,
        height,
    ];
}

/**
 * Coerce a window rectangle into the `(siiii)` tail shared by `Query` and
 * `QueryAll`. Same reasoning as {@link monitorEntry}.
 *
 * @returns {[string, number, number, number, number]|null}
 */
export function windowEntry(connector, area, rect) {
    if (typeof connector !== 'string' || connector === '')
        return null;
    if (!area || !rect || !Number.isFinite(rect.width) || !Number.isFinite(rect.height))
        return null;

    const int = value => Math.round(Number(value) || 0);
    const width = int(rect.width);
    const height = int(rect.height);
    if (width <= 0 || height <= 0)
        return null;

    return [connector, int(rect.x) - int(area.x), int(rect.y) - int(area.y), width, height];
}
