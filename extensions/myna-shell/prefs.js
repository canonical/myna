import Adw from 'gi://Adw';
import Gtk from 'gi://Gtk';

import {ExtensionPreferences} from 'resource:///org/gnome/Shell/Extensions/js/extensions/prefs.js';

import {hudStyleFromIndex, hudStyleToIndex} from './settings-logic.js';

export default class MynaPreferences extends ExtensionPreferences {
    fillPreferencesWindow(window) {
        const settings = this.getSettings();
        window._settings = settings;

        const page = new Adw.PreferencesPage({
            title: 'Appearance',
            icon_name: 'preferences-desktop-appearance-symbolic',
        });
        const group = new Adw.PreferencesGroup({
            title: 'Dictation indicator',
        });
        const row = new Adw.ComboRow({
            title: 'HUD style',
            subtitle: 'Choose the compact audio meter or animated wave ribbon',
            model: Gtk.StringList.new(['Basic', 'Wave ribbon']),
            selected: hudStyleToIndex(settings.get_string('hud-style')),
        });
        row.connect('notify::selected', () => {
            settings.set_string('hud-style', hudStyleFromIndex(row.selected));
        });
        group.add(row);
        page.add(group);
        window.add(page);
    }
}
