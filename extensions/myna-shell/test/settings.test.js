import GLib from 'gi://GLib';
import System from 'system';

import {hudStyleFromIndex, hudStyleToIndex} from '../settings-logic.js';

let failures = 0;
function check(name, condition) {
    if (condition)
        print(`ok   ${name}`);
    else {
        failures++;
        print(`FAIL ${name}`);
    }
}

const root = GLib.get_current_dir();
const decoder = new TextDecoder();
const metadata = JSON.parse(decoder.decode(
    GLib.file_get_contents(`${root}/metadata.json`)[1]));
const schema = decoder.decode(
    GLib.file_get_contents(`${root}/schemas/org.gnome.shell.extensions.myna.gschema.xml`)[1]);

check('metadata declares settings schema',
    metadata['settings-schema'] === 'org.gnome.shell.extensions.myna');
check('schema ID and path are stable',
    schema.includes('id="org.gnome.shell.extensions.myna"') &&
    schema.includes('path="/org/gnome/shell/extensions/myna/"'));
check('schema defines hud-style enum key', schema.includes('name="hud-style"'));
check('basic enum value is stable zero', schema.includes('nick="basic" value="0"'));
check('wave enum value is stable one', schema.includes('nick="wave" value="1"'));
check('schema default is basic', schema.includes("<default>'basic'</default>"));
check('basic maps to selector index zero', hudStyleToIndex('basic') === 0);
check('wave maps to selector index one', hudStyleToIndex('wave') === 1);
check('unknown selector input falls back to basic index', hudStyleToIndex('future') === 0);
check('selector zero maps to basic', hudStyleFromIndex(0) === 'basic');
check('selector one maps to wave', hudStyleFromIndex(1) === 'wave');
check('unknown selector index maps to basic', hudStyleFromIndex(99) === 'basic');

print(failures === 0 ? 'PASS settings.test.js' : `FAIL settings.test.js: ${failures} failure(s)`);
System.exit(failures === 0 ? 0 : 1);
