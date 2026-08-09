#!/usr/bin/env -S gjs -m
/*
 * Tests for the shell extension's testable halves.
 *
 *   gjs -m extension/test.js
 *
 * Covers the placement arithmetic and the D-Bus contract. It cannot cover the
 * Meta calls in extension.js — those only exist inside a running gnome-shell —
 * so extension.js is kept as thin a wrapper over these as possible.
 */

import Gio from 'gi://Gio';

import GLib from 'gi://GLib';

import {clampToWorkArea, monitorEntry, toRelative, windowEntry} from './geometry.js';
import {
    APP_BUS_NAME,
    APP_OBJECT_PATH,
    BUS_NAME,
    boolReply,
    INTERFACE_XML,
    listMonitorsReply,
    OBJECT_PATH,
    PROTOCOL_VERSION,
    queryAllReply,
    queryReply,
    SETTINGS_SCHEMA,
    WINDOW_PATH_PREFIX,
} from './interface.js';

let failures = 0;
let checks = 0;

function check(name, fn) {
    checks++;
    try {
        fn();
        print(`  ok    ${name}`);
    } catch (error) {
        failures++;
        print(`  FAIL  ${name}`);
        print(`        ${error.message}`);
    }
}

function assert(condition, message) {
    if (!condition)
        throw new Error(message ?? 'assertion failed');
}

function assertEqual(actual, expected, message) {
    const a = JSON.stringify(actual);
    const b = JSON.stringify(expected);
    if (a !== b)
        throw new Error(`${message ?? 'mismatch'}: got ${a}, expected ${b}`);
}

function source(name) {
    const [, bytes] = GLib.file_get_contents(name);
    return new TextDecoder().decode(bytes);
}

/** Source with comments removed, for checks that would otherwise read prose. */
function code(name) {
    return source(name)
        .replace(/\/\*[\s\S]*?\*\//g, '')
        .replace(/^\s*\/\/.*$/gm, '');
}

/** The modules loaded into the gnome-shell process, in load order. */
const SHELL_MODULES = [
    'extension.js',
    'service.js',
    'shortcut.js',
    'taskbarHider.js',
];

// The machine this was written on: one ultrawide, work area inset by the
// GNOME top bar and the Ubuntu dock.
const ULTRAWIDE = {x: 72, y: 37, width: 5048, height: 1403};
const LAPTOP = {x: 0, y: 1440, width: 1920, height: 1043};

print('geometry');

check('a rectangle that fits is only translated to absolute coordinates', () => {
    assertEqual(clampToWorkArea(ULTRAWIDE, 400, 300, 320, 340), {
        x: 472,
        y: 337,
        width: 320,
        height: 340,
    });
});

check('the origin maps to the work area corner, not the screen corner', () => {
    const rect = clampToWorkArea(ULTRAWIDE, 0, 0, 300, 320);
    assertEqual([rect.x, rect.y], [72, 37], 'a note at 0,0 clears the top bar and dock');
});

check('a rectangle running off the right edge is pulled back on', () => {
    const rect = clampToWorkArea(ULTRAWIDE, 5000, 0, 300, 320);
    assertEqual(rect.x, ULTRAWIDE.x + ULTRAWIDE.width - 300);
});

check('a rectangle running off the bottom edge is pulled back on', () => {
    const rect = clampToWorkArea(ULTRAWIDE, 0, 5000, 300, 320);
    assertEqual(rect.y, ULTRAWIDE.y + ULTRAWIDE.height - 320);
});

check('negative coordinates are pulled back on', () => {
    const rect = clampToWorkArea(ULTRAWIDE, -900, -900, 300, 320);
    assertEqual([rect.x, rect.y], [ULTRAWIDE.x, ULTRAWIDE.y]);
});

check('an oversized rectangle is capped to the work area', () => {
    const rect = clampToWorkArea(LAPTOP, 0, 0, 99999, 99999);
    assertEqual([rect.width, rect.height], [LAPTOP.width, LAPTOP.height]);
    assertEqual([rect.x, rect.y], [LAPTOP.x, LAPTOP.y]);
});

check('a zero or negative size becomes the smallest legal one', () => {
    assertEqual(clampToWorkArea(LAPTOP, 0, 0, 0, 0).width, 1);
    assertEqual(clampToWorkArea(LAPTOP, 0, 0, -50, -50).height, 1);
});

check('fractional input is rounded, never truncated to nonsense', () => {
    const rect = clampToWorkArea(ULTRAWIDE, 10.6, 20.4, 300.5, 320.2);
    assertEqual([rect.x, rect.y, rect.width, rect.height], [83, 57, 301, 320]);
});

check('the result is always fully inside the work area', () => {
    const cases = [
        [-9999, -9999, 300, 320],
        [9999, 9999, 300, 320],
        [0, 0, 99999, 99999],
        [2000, 700, 1, 1],
        [5047, 1402, 400, 400],
    ];
    for (const area of [ULTRAWIDE, LAPTOP]) {
        for (const [x, y, w, h] of cases) {
            const r = clampToWorkArea(area, x, y, w, h);
            assert(r.x >= area.x, `left edge off ${JSON.stringify(r)}`);
            assert(r.y >= area.y, `top edge off ${JSON.stringify(r)}`);
            assert(r.x + r.width <= area.x + area.width, `right edge off ${JSON.stringify(r)}`);
            assert(r.y + r.height <= area.y + area.height, `bottom edge off ${JSON.stringify(r)}`);
        }
    }
});

check('clamping is idempotent', () => {
    const once = clampToWorkArea(ULTRAWIDE, 9999, 9999, 300, 320);
    const [rx, ry, rw, rh] = toRelative(ULTRAWIDE, once);
    const twice = clampToWorkArea(ULTRAWIDE, rx, ry, rw, rh);
    assertEqual(twice, once, 'a placed window must not drift on re-placement');
});

check('relative and absolute coordinates round-trip', () => {
    const absolute = clampToWorkArea(ULTRAWIDE, 800, 200, 320, 340);
    assertEqual(toRelative(ULTRAWIDE, absolute), [800, 200, 320, 340]);
});

check('relative coordinates are stable when a monitor moves in the layout', () => {
    const moved = {...ULTRAWIDE, x: ULTRAWIDE.x + 1920};
    const before = clampToWorkArea(ULTRAWIDE, 800, 200, 320, 340);
    const after = clampToWorkArea(moved, 800, 200, 320, 340);

    assertEqual(toRelative(ULTRAWIDE, before), toRelative(moved, after));
    assertEqual(after.x - before.x, 1920, 'absolute position tracks the monitor');
});

print('');
print('d-bus value coercion');

check('a good monitor produces exactly the declared tuple', () => {
    const entry = monitorEntry('DP-1', 'Dell U4924DW', true, ULTRAWIDE);
    assertEqual(entry, ['DP-1', 'Dell U4924DW', true, 72, 37, 5048, 1403]);
    // The types are what GJS packs against, so check them, not just the values.
    assertEqual(typeof entry[0], 'string');
    assertEqual(typeof entry[2], 'boolean');
    for (const i of [3, 4, 5, 6]) {
        assertEqual(typeof entry[i], 'number');
        assert(Number.isInteger(entry[i]), `field ${i} must be an integer for 'i'`);
    }
});

check('a missing display name falls back to the connector', () => {
    // get_display_name() can return null; packing null into 's' throws.
    assertEqual(monitorEntry('DP-1', null, false, ULTRAWIDE)[1], 'DP-1');
    assertEqual(monitorEntry('DP-1', '', false, ULTRAWIDE)[1], 'DP-1');
    assertEqual(monitorEntry('DP-1', undefined, false, ULTRAWIDE)[1], 'DP-1');
});

check('fractional geometry is rounded to the integers the signature declares', () => {
    const entry = monitorEntry('DP-1', 'X', false, {x: 71.6, y: 37.2, width: 5048.4, height: 1403.5});
    assertEqual([entry[3], entry[4], entry[5], entry[6]], [72, 37, 5048, 1404]);
});

check('unusable monitors are dropped, not packed as garbage', () => {
    // Each of these used to reach _packVariant and fail the entire call with
    // "Service implementation returned an incorrect value type".
    assertEqual(monitorEntry(null, 'X', true, ULTRAWIDE), null, 'null connector');
    assertEqual(monitorEntry(undefined, 'X', true, ULTRAWIDE), null, 'undefined connector');
    assertEqual(monitorEntry('', 'X', true, ULTRAWIDE), null, 'empty connector');
    assertEqual(monitorEntry('DP-1', 'X', true, undefined), null, 'no work area');
    assertEqual(monitorEntry('DP-1', 'X', true, {}), null, 'empty work area');
    assertEqual(monitorEntry('DP-1', 'X', true, {x: 0, y: 0, width: 0, height: 0}), null, 'zero size');
    assertEqual(
        monitorEntry('DP-1', 'X', true, {x: 0, y: 0, width: NaN, height: 100}),
        null,
        'NaN size'
    );
});

check('truthiness is coerced to a real boolean', () => {
    // `index === primary` is a boolean, but a refactor to a truthy value would
    // pack as the wrong type for 'b'.
    assertEqual(monitorEntry('DP-1', 'X', 1, ULTRAWIDE)[2], true);
    assertEqual(monitorEntry('DP-1', 'X', 0, ULTRAWIDE)[2], false);
    assertEqual(monitorEntry('DP-1', 'X', undefined, ULTRAWIDE)[2], false);
});

check('window entries are monitor-relative and integral', () => {
    const rect = {x: 472, y: 337, width: 320, height: 340};
    assertEqual(windowEntry('DP-1', ULTRAWIDE, rect), ['DP-1', 400, 300, 320, 340]);
    assertEqual(windowEntry('DP-1', ULTRAWIDE, null), null);
    assertEqual(
        windowEntry('DP-1', ULTRAWIDE, {x: 0, y: 0, width: 0, height: 0}),
        null,
        'an unmapped window reports 0x0 and must not be reported as real'
    );
});

print('');
print('reply shapes');

/** The reply signature the interface XML declares for a method. */
function declaredReplySignature(method) {
    const iface = Gio.DBusNodeInfo.new_for_xml(INTERFACE_XML).interfaces[0];
    const info = iface.lookup_method(method);
    assert(info, `no such method ${method}`);
    return `(${info.out_args.map(a => a.signature).join('')})`;
}

check('every reply matches the signature its method declares', () => {
    // The bug this exists for: GJS wraps a single-out-arg return in an array
    // *itself*, so returning `[monitors]` became `[[monitors]]` and failed with
    // "Service implementation returned an incorrect value type", naming neither
    // the method nor the field. Building variants explicitly removes the
    // guesswork; comparing against the XML proves the two agree.
    const monitor = monitorEntry('DP-1', 'Dell', true, ULTRAWIDE);
    // (ssiiii): object path *and* connector, then the rectangle.
    const window = ['/us/hagreli/Stickies/window/1', 'DP-1', 10, 20, 300, 320];

    const cases = [
        ['ListMonitors', listMonitorsReply([monitor])],
        ['ListMonitors', listMonitorsReply([])],
        ['QueryAll', queryAllReply([window])],
        ['QueryAll', queryAllReply([])],
        ['Query', queryReply(['DP-1', 10, 20, 300, 320])],
        ['Query', queryReply(null)],
        ['Place', boolReply(true)],
        ['SetPinned', boolReply(false)],
    ];

    for (const [method, reply] of cases) {
        assertEqual(
            reply.get_type_string(),
            declaredReplySignature(method),
            `${method} reply shape`
        );
    }
});

check('replies survive a round trip with their values intact', () => {
    const monitor = monitorEntry('DP-1', 'Dell U4924DW', true, ULTRAWIDE);
    const [monitors] = listMonitorsReply([monitor]).deepUnpack();
    assertEqual(monitors.length, 1);
    assertEqual(monitors[0], ['DP-1', 'Dell U4924DW', true, 72, 37, 5048, 1403]);

    const found = queryReply(['DP-1', 10, 20, 300, 320]).deepUnpack();
    assertEqual(found, [true, 'DP-1', 10, 20, 300, 320]);
    assertEqual(queryReply(null).deepUnpack(), [false, '', 0, 0, 0, 0]);

    assertEqual(boolReply(true).deepUnpack(), [true]);
    // Truthy-but-not-boolean must still pack as 'b'.
    assertEqual(boolReply(1).deepUnpack(), [true]);
    assertEqual(boolReply(undefined).deepUnpack(), [false]);
});

check('handlers return variants, not bare values', () => {
    // Belt and braces: if a future method returns a plain array again, GJS's
    // wrapping rule applies once more and the failure is just as opaque.
    const [, bytes] = GLib.file_get_contents('extension.js');
    const source = new TextDecoder().decode(bytes);
    const bareReturns = [...source.matchAll(/^\s{8}return \[/gm)];
    assert(
        bareReturns.length === 0,
        `${bareReturns.length} D-Bus method(s) return a bare array; use a *Reply() builder`
    );
});

print('');
print('d-bus contract');

check('the interface XML parses', () => {
    const node = Gio.DBusNodeInfo.new_for_xml(INTERFACE_XML);
    assert(node.interfaces.length === 1, 'exactly one interface');
    assertEqual(node.interfaces[0].name, 'us.hagreli.Stickies.Shell');
});

check('every method the Rust client calls exists with the right signature', () => {
    const iface = Gio.DBusNodeInfo.new_for_xml(INTERFACE_XML).interfaces[0];
    const signature = method => {
        const info = iface.lookup_method(method);
        assert(info, `missing method ${method}`);
        const inArgs = info.in_args.map(a => a.signature).join('');
        const outArgs = info.out_args.map(a => a.signature).join('');
        return `${inArgs}->${outArgs}`;
    };

    // These must match the VariantTy strings in src/placement.rs. D-Bus wraps
    // every reply in a tuple, which is why the Rust side asks for "(...)".
    assertEqual(signature('ListMonitors'), '->a(ssbiiii)');
    assertEqual(signature('Place'), 'ssiiii->b');
    assertEqual(signature('Query'), 's->bsiiii');
    assertEqual(signature('QueryAll'), '->a(ssiiii)');
    assertEqual(signature('SetPinned'), 'sb->b');
});

check('the MonitorsChanged signal is declared', () => {
    const iface = Gio.DBusNodeInfo.new_for_xml(INTERFACE_XML).interfaces[0];
    assert(iface.lookup_signal('MonitorsChanged'), 'missing MonitorsChanged');
});

check('the Version property is a readable u32', () => {
    const iface = Gio.DBusNodeInfo.new_for_xml(INTERFACE_XML).interfaces[0];
    const version = iface.lookup_property('Version');
    assert(version, 'missing Version property');
    assertEqual(version.signature, 'u');
});

check('the names match what the Rust client expects', () => {
    // Mirrors the constants in src/placement.rs; a rename on one side without
    // the other is silent at runtime (the app just never finds the extension).
    assertEqual(BUS_NAME, 'us.hagreli.Stickies.Shell');
    assertEqual(OBJECT_PATH, '/us/hagreli/Stickies/Shell');
    assertEqual(PROTOCOL_VERSION, 1);
});

check('the window path prefix is scoped to the Stickies application ID', () => {
    // The security boundary: this prefix is derived by GTK from the app ID, so
    // no other application can produce a path that starts with it.
    assertEqual(WINDOW_PATH_PREFIX, '/us/hagreli/Stickies/window/');
    assert(
        !'/org/gnome/Nautilus/window/1'.startsWith(WINDOW_PATH_PREFIX),
        'another app\'s window must not match'
    );
    assert(
        !'/us/hagreli/StickiesEvil/window/1'.startsWith(WINDOW_PATH_PREFIX),
        'a lookalike application ID must not match'
    );
    assert(
        '/us/hagreli/Stickies/window/12'.startsWith(WINDOW_PATH_PREFIX),
        'our own windows must match'
    );
});

print('');
print('settings schema');

check('every gi:// namespace used is actually imported', () => {
    // Nothing here runs outside gnome-shell, so a missing import is invisible
    // until enable() throws in a live session — which costs a logout to
    // discover. This is the cheap static substitute, and it has to cover every
    // shell module: splitting one file into four multiplied the places an
    // import can go missing.
    for (const name of SHELL_MODULES) {
        const text = code(name);

        const imported = new Set(
            [...text.matchAll(/^import (\w+) from 'gi:\/\//gm)].map(m => m[1])
        );
        imported.add('Main'); // namespace import from a shell resource

        const used = new Set(
            [...text.matchAll(/\b(Gio|GLib|GObject|Meta|Shell|Clutter|St|Main)\./g)].map(m => m[1])
        );

        for (const namespace of used) {
            assert(
                imported.has(namespace),
                `${name}: ${namespace}. is used but never imported — enable() will throw`
            );
        }
    }
});

check('no shell module imports a toolkit that belongs to prefs', () => {
    // extension.js and prefs.js are different processes; Gtk, Gdk or Adw in
    // this half conflicts with Clutter and is an outright EGO rejection.
    for (const name of [...SHELL_MODULES, 'geometry.js', 'interface.js']) {
        assert(
            !/from 'gi:\/\/(Gtk|Gdk|Adw)'/.test(source(name)),
            `${name} must not import Gtk, Gdk or Adw`
        );
    }
});

check('metadata declares the settings schema getSettings() needs', () => {
    // Extension.getSettings() with no argument reads metadata['settings-schema'].
    // Omitting it does not fail until enable() runs inside a live shell, where
    // it surfaces as the opaque "Expected type string for argument
    // 'schema_id' but got type undefined" and the extension refuses to load.
    const [, bytes] = GLib.file_get_contents('metadata.json');
    const meta = JSON.parse(new TextDecoder().decode(bytes));
    assertEqual(
        meta['settings-schema'],
        SETTINGS_SCHEMA,
        'metadata.json settings-schema must match the schema id'
    );
});

check('the schema compiles and declares both keys', () => {
    // Compiled into a temporary directory so this never depends on the
    // extension being installed.
    const [, out, err, status] = GLib.spawn_command_line_sync(
        `glib-compile-schemas --dry-run --strict ${GLib.get_current_dir()}/schemas`
    );
    assert(
        status === 0,
        `glib-compile-schemas rejected the schema: ${new TextDecoder().decode(err) || new TextDecoder().decode(out)}`
    );

    const [ok, bytes] = GLib.file_get_contents(
        'schemas/org.gnome.shell.extensions.stickies.gschema.xml'
    );
    assert(ok, 'could not read the schema');
    const xml = new TextDecoder().decode(bytes);

    assert(xml.includes(`id="${SETTINGS_SCHEMA}"`), 'schema id must match interface.js');
    assert(xml.includes('name="new-note"'), 'missing the new-note shortcut key');
    assert(xml.includes('name="hide-from-taskbar"'), 'missing the taskbar key');
});

check('the shortcut key is an "as", as addKeybinding requires', () => {
    const [, bytes] = GLib.file_get_contents(
        'schemas/org.gnome.shell.extensions.stickies.gschema.xml'
    );
    const xml = new TextDecoder().decode(bytes);
    // Main.wm.addKeybinding reads a string array; any other type throws at
    // enable() time and the extension fails to load with no shortcut at all.
    assert(
        /<key name="new-note" type="as">/.test(xml),
        'new-note must be type "as"'
    );
    assert(xml.includes('<Super><Shift>n'), 'default accelerator missing');
});

check('the shortcut targets the app\'s own action group', () => {
    // The extension calls org.gtk.Actions.Activate on the running app rather
    // than spawning the binary, so a note lands in the existing instance.
    assertEqual(APP_BUS_NAME, 'us.hagreli.Stickies');
    assertEqual(APP_OBJECT_PATH, '/us/hagreli/Stickies');

    const text = source('shortcut.js');
    assert(text.includes("'org.gtk.Actions'"), 'must call org.gtk.Actions');
    assert(text.includes("'Activate'"), 'must use the Activate method');
    assert(
        text.includes("'(sava{sv})'"),
        'org.gtk.Actions.Activate takes (sava{sv}); a wrong signature fails silently'
    );
});

check('taskbar hiding uses the method, not the read-only property', () => {
    // Meta.Window:skip-taskbar is read-only; assigning to it fails silently,
    // which is indistinguishable from "this Mutter does not support it".
    // hide_from_window_list()/show_in_window_list() are the real API.
    const text = source('taskbarHider.js');

    assert(
        !/\.skip_taskbar\s*=/.test(text),
        'assigning to skip_taskbar is a silent no-op; call hide_from_window_list()'
    );
    assert(text.includes('hide_from_window_list'), 'must hide via the method');
    assert(
        text.includes('show_in_window_list'),
        'destroy() must put windows back in the dock'
    );
});

check('everything enable() creates is torn down in disable()', () => {
    const text = source('extension.js');

    for (const field of ['_service', '_shortcut', '_hider', '_settings']) {
        assert(
            text.includes(`this.${field} = null`),
            `disable() must release ${field}`
        );
    }
    assert(
        text.includes('this._settings.disconnect(this._hideChangedId)'),
        'the settings handler outlives disable() unless it is disconnected'
    );
    assert(
        source('shortcut.js').includes('removeKeybinding'),
        'a keybinding left registered survives disable and blocks the key'
    );
});

check('no module hedges over shell versions', () => {
    // The EGO best practices reject `typeof x === 'function'` and `?.()` on
    // guaranteed APIs — they are what gets written when hedging across shell
    // versions nobody chose to target. Every Meta method used here has been in
    // Mutter since well before the declared floor.
    for (const name of SHELL_MODULES) {
        const text = source(name);
        assert(
            !/typeof \w+(\.\w+)+ [!=]== 'function'/.test(text),
            `${name}: probing for a method that is always there`
        );
        assert(
            !/\bget_gtk_window_object_path\?\./.test(text),
            `${name}: get_gtk_window_object_path is a plain Meta.Window method`
        );
    }
});

print('');
if (failures > 0) {
    print(`${failures} of ${checks} checks failed`);
    imports.system.exit(1);
}
print(`all ${checks} checks passed`);
