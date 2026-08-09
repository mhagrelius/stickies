/*
 * Stickies Window Placement — GNOME Shell extension.
 *
 * The Stickies app cannot position its own windows: Wayland has no client-side
 * positioning request, Mutter does not implement wlr-layer-shell, and GTK 4
 * removed gtk_window_move. This extension runs inside the compositor, where
 * those operations are available, and exposes exactly the handful of them that
 * Stickies needs on the session bus.
 *
 * Scope is deliberately narrow — service.js refuses to touch any window whose
 * GTK object path does not sit under Stickies' own prefix.
 *
 * The arithmetic lives in geometry.js and the contract in interface.js, both
 * free of shell imports so they can be tested outside a live session — see
 * `gjs -m extension/test.js`.
 */

import { Extension } from 'resource:///org/gnome/shell/extensions/extension.js';

import { GlobalShortcut } from './shortcut.js';
import { StickiesService } from './service.js';
import { TaskbarHider } from './taskbarHider.js';

export default class StickiesExtension extends Extension {
    enable() {
        this._settings = this.getSettings();

        this._hider = this._settings.get_boolean('hide-from-taskbar')
            ? new TaskbarHider()
            : null;
        this._service = new StickiesService(this._hider);
        this._shortcut = new GlobalShortcut(this._settings);

        this._hideChangedId = this._settings.connect('changed::hide-from-taskbar',
            () => this._applyHiding());
    }

    disable() {
        this._settings.disconnect(this._hideChangedId);
        this._hideChangedId = 0;

        this._shortcut.destroy();
        this._shortcut = null;

        this._service.destroy();
        this._service = null;

        this._hider?.destroy();
        this._hider = null;

        this._settings = null;
    }

    /** Build or tear down the hider so the setting takes without a re-enable. */
    _applyHiding() {
        const wanted = this._settings.get_boolean('hide-from-taskbar');
        if (wanted === Boolean(this._hider))
            return;

        this._hider?.destroy();
        this._hider = wanted ? new TaskbarHider() : null;
        this._service.setHider(this._hider);
    }
}
