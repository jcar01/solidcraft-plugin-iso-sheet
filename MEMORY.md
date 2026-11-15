# Session memory

Notes kept in the repo so a later session (human or agent) can pick this up without re-deriving
it. Newest entry first.

---

## 2026-11-14 — iso-sheet 1.1.0: form widgets the host can and cannot draw

### Request

Replace four text inputs with richer controls: a multi-select Bodies dropdown populated from the
open design, a calendar Date picker defaulting to today, a folder Browse button for Write SVG to,
and a standard Scale dropdown.

### What SolidCraft 0.42.0 actually supports

The installed app is **0.42.0** now (`solidcraft-cli --version`), not the pre-0.42.0 build noted
below. Its plugin dialog (`crates/ui-egui/src/plugin_menu.rs`) draws:

- `number`, `integer`, `boolean`, `string` as text;
- `string` with `enum` as `Kind::Choice` → `ui_kit::dropdown` (confirmed by the
  `plugin_dialog_choose` and `plugin_field_` strings in `solidcraft.exe`).

`field()` allows only these keywords: for `string` + `enum`, just `enum`; for `string` without
`enum`, `minLength`, `maxLength`, `format`; plus annotations such as `title`, `description`,
`default`. Any other keyword (`items`, `oneOf`, `$ref`, …) makes `form()` return `None` and Run stays
off ("This entry needs to be run from the agent or CLI").

Still impossible in 0.42.0:

- **Dynamic body list / multi-select.** The form is built from the static manifest JSON before any
  plugin code runs; there is no callback to fetch body names. Arrays are rejected by the form, and
  `validate_list_inputs()` still forces `bodies` to be an array.
- **Date picker.** `format` is accepted as an annotation but ignored; there is no calendar widget.
- **Folder picker.** No Browse control; `format` is ignored. `output` is a path inside a host-approved
  file grant, and this version has no user UI for choosing plugin folder grants.

Upstream: issue **#16274** ("Plugins: support interactive selection fields…") is still open. PR
**#16281** is a docs-only proposal, not an implementation. `commands@0.1` still has no `ui`
capability (`CAPABILITIES` in `crates/craft-plugin/src/lib.rs` is only `("commands", (0, 1))`).

### What 1.1.0 does

- `plugin.json`
  - `scale` is a real `string` + `enum` dropdown: `Auto`, `1:1`, `1:2`, `1:5`, `1:10`, `1:20`,
    `1:50`, `1:100`; default `Auto`.
  - `date` gets `format: "date"` and `output` gets `format: "path"` — ignored by 0.42.0, but
    forward-compatible if a later host implements the widgets.
  - `permissions.clock: true` so an empty date can become today's date.
  - Version `1.1.0`.
- `src/lib.rs`
  - `parse_scale()` accepts `Auto`/empty/`1:2`/`2` (legacy denominator) and `2:1` (enlargement);
    rejects zero, negative and garbage.
  - `read_date()` enforces real `YYYY-MM-DD` dates (leap years included); empty means
    `clock.now()` (Unix seconds) → `civil_from_days()`.
  - `output_name()` accepts a file name, adds `.svg` when the extension is missing, and turns a
    folder ending in `/` or `\` into `iso-sheet.svg` inside it.
  - Requested body names are matched case-insensitively against `document.inspect` and errors list
    the available names; duplicates are removed.
  - Auto scale now considers up to `1:100`; explicit enlargements display as `2:1`; a drawing that
    overflows the sheet returns `scale_note`.
  - Result now also carries `date` and `available_bodies`.
- `README.md` documents the dropdown and the three remaining host limitations.
- Unit tests for date validation, scale parsing and output paths (3 tests).

### Verification

- `cargo test` → 3 passed.
- `cargo component build --release --target wasm32-unknown-unknown` → clean.
- `plugin.inspect` on the staged package → manifest valid, version 1.1.0, `within_host_limits`.
- Headless MCP (`plugin.run`) on `%TEMP%\opencode\iso-test-small.sc` (bodies `Plate` 40×30×20 and
  `Post` 10×10×25):
  - `body_names: "plate"` → only `Plate`; `available_bodies: ["Plate","Post"]`.
  - `body_names: ""`, `scale: "Auto"` → both bodies, `scale: "1:1"`.
  - `scale: "1:2"` → `scale: "1:2"`; `scale: "2:1"` → `scale: "2:1"`.
  - `date: ""` → `date: "2026-11-14"`.
  - `notes: "TEST ONE; TEST TWO"` → both notes in the SVG.
  - `body_names: "nope"` → `invalid input body_names: unknown body 'nope'; available: Plate, Post`.
  - `date: "11/14/2026"` → `invalid input date: use YYYY-MM-DD, for example 2026-11-14`.
  - `scale: "bogus"` → `invalid input scale: scale must be Auto, 1:2, or a positive number`.

### Gotchas from this round

- **`output` set with no grant is a hard failure**, not the inline fallback:
  `plugin: permission denied: files.write:output`. The host poisons the transaction, so the guest's
  `match` around `files::write_file` cannot recover. Leave `output` empty when no folder is granted.
- The form sorts fields by required order then key, so `scale` stays between `output` and `title`.
- `clock.now()` returns `0` without the clock permission; `today()` then leaves the date blank
  rather than printing 1970-01-01.
- `format` does not change the widget in 0.42.0. Do not expect `format: "date"` or `"path"` to add a
  calendar or Browse button until the host implements it.

### Install state on this machine

- Installed package: `%LOCALAPPDATA%\SolidCraft\plugins\iso-sheet\` → `plugin.json` +
  `iso-sheet.wasm` at 1.1.0. Wasm SHA-256 starts `778290E60D736338`.
- Backup of 1.0.1: `%TEMP%\opencode\iso-sheet-1.0.1-backup\`.
- App reads the plugins folder once per session: restart SolidCraft after the manual copy.
- `iso-sheet.zip` regenerated (82,496 bytes) from `plugin.json`, `iso-sheet.wasm` and the two
  licenses.

### Repo state at end of session

- Commit `6cbfc5f` "Add standard scale dropdown and robust form values - 1.1.0" on `main`.
- `src/bindings.rs` remains generated by `cargo component` and intentionally untracked.

---

## 2026-11-14 — iso-sheet 1.0.0 → 1.0.1: the app dialog could not run the plugin

### Symptom

Opening the installed plugin (Plugins menu › *Isometric Sheet*, or Model › Plugins › *Iso Sheet*)
showed the description, but **Run was greyed out** with:

> This entry needs to be run from the agent or CLI.

Running it through the agent/MCP (`iso-sheet.sheet`) worked fine.

### Root cause (SolidCraft app, not this repo)

Two rules in the host contradict each other for any entry that names bodies:

1. `crates/ui-egui/src/plugin_menu.rs` — `form()` only draws **scalar** properties
   (`string`, `number`, `integer`, `boolean`, plus `enum` on strings). Any array property, or any
   keyword outside the accepted set (`items`, `minItems`, `oneOf`, `$ref`, …), makes `form()`
   return `None`; the dialog then shows the message above and disables Run. Run only turns on for
   schemas the dialog can draw and check.
2. `crates/craft-plugin/src/lib.rs` — `validate_list_inputs()` forces properties named exactly
   `selection`, `target`, `targets`, `bodies` or `body_ids` to have `"type": "array"`, or the
   manifest is rejected at load time.

So 1.0.0's schema (`bodies`: array, required by rule 2; `notes`: array, rejected by rule 1) could
never render a form. Upstream tracking: tangency-labs/solidcraft issue **#16274**
("Plugins: support interactive selection fields…"), PR #16281 in progress. Reference source used:
commit `4b0bf7d178d6257049233bd3a68c7d3ea4175f42`.

### Fix (1.0.1)

- `plugin.json`
  - `bodies` → **`body_names`**, type `string` (comma-separated). The rename is what dodges the
    reserved-name rule; the type change is what lets the dialog draw it.
  - `notes` → type `string`, separated by `;`.
  - Version `1.0.1`.
- `src/lib.rs` (`read_opts`)
  - Small `list()` helper accepts a JSON string *or* an array for both fields.
  - `body_names` is read first, falling back to `bodies` (old agent/CLI callers keep working).
  - Strings split on `,` `;` newline for bodies, `;` `|` newline for notes; trimmed, empties dropped.
  - An empty body list means "every body", not "no bodies".
- `Cargo.toml` version `1.0.1`; `README.md` inputs table and a paragraph explaining the dialog
  limitation.
- `iso-sheet.zip` regenerated from the new build.

### Verification (agent, against the open IAR-823 Jack Adapter design)

- `plugin.inspect` on the staged package: manifest valid, version 1.0.1, all ten fields scalar.
- `plugin.run` (`path` = staged package, `entry` = `sheet`):
  - `{"body_names": "Tube, Rod", "notes": "TEST ONE; TEST TWO"}` → result bodies exactly
    `Tube`, `Rod`; both notes present in the SVG; `ok: true`; sheet returned inline because no
    output folder is granted in the agent session.
- Default/empty params still draw every body (Tube, Spool, Rod, Washer, Cap).

### Install state on this machine

- Installed package: `%LOCALAPPDATA%\SolidCraft\plugins\iso-sheet\` → `plugin.json` + `iso-sheet.wasm`
  at 1.0.1 (wasm SHA-256 starts `A75C724283848686`).
- Backup of 1.0.0: `%TEMP%\opencode\iso-sheet-1.0.0-backup\`.
- The app reads the plugins folder **once per session**, so a restart is required after copying
  files by hand. The in-app installer appears immediately.

### Gotchas worth remembering

- The agent/MCP session cannot install into the plugins folder: `plugin.install` fails with
  *"outside the folders this agent may use"*. Copy the two files with the OS shell, then restart,
  or install the zip from `Preferences › Plugins › Install plugin…`.
- `plugin.inspect` requires the package **folder name to equal the manifest `id`**
  (`iso-sheet`), and only works for paths inside the agent's allowed folders.
- `plugin.run` takes `params` as a JSON **value**. Passing a JSON *string* is double-encoded and
  the plugin silently falls back to defaults — that produced a confusing "all bodies" first test.
- `plugin.list` can return `[]` while the plugin is in fact loaded; confirm with
  `list_commands` (look for `iso-sheet.sheet`) instead.
- The host app in this session was pre-0.42.0 with an "Update to 0.42.0" badge; re-check whether
  0.42.0+ relaxes the array/form rules before assuming this workaround is still needed.

### Repo state at end of session

- Commit `383ac55` "Fix app dialog run: scalar inputs only (body_names, notes) - 1.0.1" on `main`.
- `src/bindings.rs` is generated by `cargo component` and intentionally untracked.
- Build: `cargo component build --release --target wasm32-unknown-unknown`, copy
  `target/wasm32-unknown-unknown/release/iso_sheet.wasm` to `iso-sheet.wasm`.
