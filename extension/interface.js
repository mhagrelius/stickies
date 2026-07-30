/*
 * The D-Bus contract between Stickies and this extension.
 *
 * Kept in its own module with no gnome-shell imports so it can be validated
 * outside a live session (`gjs -m extension/test.js`). The Rust client's
 * matching constants live in src/placement.rs — change both together.
 */

export const BUS_NAME = 'us.hagreli.Stickies.Shell';
export const OBJECT_PATH = '/us/hagreli/Stickies/Shell';

/**
 * Only windows exported under this prefix may be touched. GTK derives the path
 * from the application ID, so a window can carry it only if it belongs to an
 * application whose ID is us.hagreli.Stickies. This is the extension's entire
 * security boundary.
 */
export const WINDOW_PATH_PREFIX = '/us/hagreli/Stickies/window/';

/** Bumped together with PROTOCOL_VERSION in src/placement.rs. */
export const PROTOCOL_VERSION = 1;

/** The application this extension exists to serve. */
export const APP_BUS_NAME = 'us.hagreli.Stickies';
export const APP_OBJECT_PATH = '/us/hagreli/Stickies';

/** GSettings schema holding the global shortcut and the taskbar option. */
export const SETTINGS_SCHEMA = 'org.gnome.shell.extensions.stickies';

export const INTERFACE_XML = `
<node>
  <interface name="us.hagreli.Stickies.Shell">
    <!-- Attached monitors: connector, display name, primary, and the work
         area in absolute coordinates (panels and docks already subtracted). -->
    <method name="ListMonitors">
      <arg type="a(ssbiiii)" direction="out" name="monitors"/>
    </method>

    <!-- Move and resize one note window. x/y are relative to the named
         monitor's work area; the window is clamped to stay fully on it. -->
    <method name="Place">
      <arg type="s" direction="in" name="objectPath"/>
      <arg type="s" direction="in" name="connector"/>
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/>
      <arg type="i" direction="in" name="height"/>
      <arg type="b" direction="out" name="placed"/>
    </method>

    <!-- Where one note window currently is, in monitor-relative coordinates. -->
    <method name="Query">
      <arg type="s" direction="in" name="objectPath"/>
      <arg type="b" direction="out" name="found"/>
      <arg type="s" direction="out" name="connector"/>
      <arg type="i" direction="out" name="x"/>
      <arg type="i" direction="out" name="y"/>
      <arg type="i" direction="out" name="width"/>
      <arg type="i" direction="out" name="height"/>
    </method>

    <!-- The same, for every mapped note window, in one round trip. -->
    <method name="QueryAll">
      <arg type="a(ssiiii)" direction="out" name="windows"/>
    </method>

    <!-- Keep a note above other windows. -->
    <method name="SetPinned">
      <arg type="s" direction="in" name="objectPath"/>
      <arg type="b" direction="in" name="pinned"/>
      <arg type="b" direction="out" name="ok"/>
    </method>

    <!-- Emitted when monitors are plugged, unplugged or rearranged, so the
         app can re-resolve every note against the new layout. -->
    <signal name="MonitorsChanged"/>

    <property name="Version" type="u" access="read"/>
  </interface>
</node>`;

/*
 * Reply builders.
 *
 * Every method returns an explicit GLib.Variant rather than a plain JS value.
 * GJS will happily pack a plain value for you, but the rule is subtle: with
 * exactly one out argument it wraps the value in an array *itself*
 * (Gio.js `_handleDBusReply`, "if one arg, we don't require the handler
 * wrapping it into an Array"), and with several it expects the array from you.
 * Returning `[monitors]` from a one-out-arg method therefore becomes
 * `[[monitors]]` and fails with the uninformative
 *
 *     Service implementation returned an incorrect value type
 *
 * naming neither the method nor the field. Building the variant here removes
 * the guesswork — `_handleDBusReply` passes a Variant straight through — and
 * makes the shapes testable with plain gjs, no compositor required.
 */

import GLib from 'gi://GLib';

/** `(a(ssbiiii))` — the reply to ListMonitors. */
export function listMonitorsReply(monitors) {
    return new GLib.Variant('(a(ssbiiii))', [monitors]);
}

/** `(bsiiii)` — the reply to Query. `entry` is null when not found. */
export function queryReply(entry) {
    return entry
        ? new GLib.Variant('(bsiiii)', [true, ...entry])
        : new GLib.Variant('(bsiiii)', [false, '', 0, 0, 0, 0]);
}

/** `(a(ssiiii))` — the reply to QueryAll. */
export function queryAllReply(windows) {
    return new GLib.Variant('(a(ssiiii))', [windows]);
}

/** `(b)` — the reply to Place and SetPinned. */
export function boolReply(value) {
    return new GLib.Variant('(b)', [Boolean(value)]);
}
