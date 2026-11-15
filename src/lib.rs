// iso-sheet — SolidCraft plugin: isometric manufacturing sheet.
//
// Reads the open design through the app command surface, projects every body to a
// true isometric, removes hidden lines with a depth buffer, and writes an SVG sheet
// with a title block, bill of materials, parameter table and overall dimensions.

use std::collections::HashMap;

#[allow(warnings)]
mod bindings;

use bindings::craft::plugin::clock;
use bindings::craft::plugin::commands;
use bindings::craft::plugin::control;
use bindings::craft::plugin::files;
use bindings::craft::plugin::log::{log as host_log, Level};
use bindings::craft::plugin::types::{FieldFailure, PluginError};
use bindings::exports::craft::plugin::entry::Guest;
use serde_json::Value;

struct IsoSheet;
bindings::export!(IsoSheet with_types_in bindings);

type P3 = [f64; 3];

// ---------------------------------------------------------------- small math
fn sub(a: P3, b: P3) -> P3 { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn cross(a: P3, b: P3) -> P3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: P3, b: P3) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn norm(a: P3) -> P3 {
    let l = dot(a, a).sqrt();
    if l <= 1e-12 { [0.0, 0.0, 0.0] } else { [a[0] / l, a[1] / l, a[2] / l] }
}

// isometric camera basis
const YAW: f64 = -std::f64::consts::PI / 4.0;
const PITCH: f64 = 0.615_479_708_670_387_4; // atan(1/sqrt(2))

fn basis() -> (P3, P3, P3) {
    let (sy, cy) = (YAW.sin(), YAW.cos());
    let (sp, cp) = (PITCH.sin(), PITCH.cos());
    let right = [-sy, cy, 0.0];
    let up = [-sp * cy, -sp * sy, cp];
    let dir = [cp * cy, cp * sy, sp]; // toward the viewer
    (right, up, dir)
}

fn project(p: P3, right: P3, up: P3, dir: P3) -> (f64, f64, f64) {
    (dot(p, right), dot(p, up), dot(p, dir))
}

// ---------------------------------------------------------------- host calls
fn call(cmd: &str, args: &Value) -> Result<Value, PluginError> {
    let text = commands::run(cmd, &args.to_string()).map_err(|e| match e {
        PluginError::CommandFailed(mut f) => {
            if f.command.is_empty() { f.command = cmd.to_string(); }
            PluginError::CommandFailed(f)
        }
        other => other,
    })?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| PluginError::Trap(format!("{cmd} returned invalid JSON: {e}")))?;
    // a command surface may wrap in {results:[...]}
    if let Some(first) = v.get("results").and_then(|r| r.as_array()).and_then(|a| a.first()) {
        return Ok(first.clone());
    }
    Ok(v)
}

fn note(msg: &str) { host_log(Level::Info, msg); }

fn invalid(field: &str, reason: &str) -> PluginError {
    PluginError::InvalidInput(FieldFailure { field: field.to_string(), reason: reason.to_string() })
}

// ---------------------------------------------------------------- form values
/// A scale is the denominator of 1:N (`2` or `1:2`), or a numerator when it is larger than the
/// model (`2:1`). `Auto` and an empty value leave it to the sheet.
fn parse_scale(value: Option<&Value>) -> Result<Option<f64>, PluginError> {
    let Some(value) = value else { return Ok(None); };
    if value.is_null() {
        return Ok(None);
    }
    if let Some(n) = value.as_f64() {
        return if n.is_finite() && n > 0.0 {
            Ok(Some(n))
        } else {
            Err(invalid("scale", "scale must be Auto, 1:2, or a positive number"))
        };
    }
    let Some(text) = value.as_str().map(str::trim) else {
        return Err(invalid("scale", "scale must be Auto, 1:2, or a positive number"));
    };
    if text.is_empty() || text.eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    let Some((numer, denom)) = text.split_once(':') else {
        return match text.parse::<f64>() {
            Ok(n) if n.is_finite() && n > 0.0 => Ok(Some(n)),
            _ => Err(invalid("scale", "scale must be Auto, 1:2, or a positive number")),
        };
    };
    let numer: f64 = numer.trim().parse().map_err(|_| invalid("scale", "use a ratio such as 1:2"))?;
    let denom: f64 = denom.trim().parse().map_err(|_| invalid("scale", "use a ratio such as 1:2"))?;
    if !(numer.is_finite() && denom.is_finite()) || numer <= 0.0 || denom <= 0.0 {
        return Err(invalid("scale", "scale terms must be positive numbers"));
    }
    Ok(Some(denom / numer))
}

fn valid_date(text: &str) -> bool {
    if text.len() != 10 || !text.is_ascii() {
        return false;
    }
    let bytes = text.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (Ok(year), Ok(month), Ok(day)) = (
        text[0..4].parse::<i32>(),
        text[5..7].parse::<u32>(),
        text[8..10].parse::<u32>(),
    ) else {
        return false;
    };
    if !(1..=12).contains(&month) || day == 0 {
        return false;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let last = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day <= last
}

fn today() -> Option<String> {
    // The host returns Unix seconds; without the clock permission it returns the epoch.
    let secs = clock::now();
    (secs > 0).then(|| civil_from_days((secs / 86_400) as i64))
}

fn read_date(value: Option<&Value>) -> Result<String, PluginError> {
    let raw = value.and_then(Value::as_str).unwrap_or("").trim();
    if raw.is_empty() {
        return Ok(match today() {
            Some(date) => date,
            None => {
                note("clock permission is off; leaving the sheet date blank");
                String::new()
            }
        });
    }
    if !valid_date(raw) {
        return Err(invalid("date", "use YYYY-MM-DD, for example 2026-11-14"));
    }
    Ok(raw.to_string())
}

/// The SVG path inside the plugin's `output` grant. A folder ending in `/` or `\` gets the
/// default file name; a name without an extension gets `.svg`.
fn output_name(value: Option<&str>) -> Option<String> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    let mut name = raw.to_string();
    if name.ends_with('/') || name.ends_with('\\') {
        name.push_str("iso-sheet.svg");
    } else if std::path::Path::new(&name).extension().is_none() {
        name.push_str(".svg");
    }
    Some(name)
}

// ---------------------------------------------------------------- options
struct Opts {
    bodies: Option<Vec<String>>,
    hidden: bool,
    tolerance: f64,
    title: String,
    dwg: String,
    out: Option<String>,
    inline: bool,
    scale: Option<f64>,
    notes: Vec<String>,
    date: String,
}

fn read_opts(params: &str) -> Result<Opts, PluginError> {
    let v: Value = if params.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(params).map_err(|e| invalid("params", &e.to_string()))?
    };
    // The app's plugin dialog draws scalar fields and a string `enum` as a dropdown; an array in
    // the schema disables the whole form, and the manifest itself forces `bodies` to be an array.
    // Arrays stay accepted for agent/CLI callers.
    fn list(value: Option<&Value>, separators: &[char]) -> Option<Vec<String>> {
        match value? {
            Value::String(text) => Some(
                text.split(|c| separators.contains(&c))
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect(),
            ),
            Value::Array(items) => Some(
                items.iter().filter_map(|x| x.as_str()).map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect(),
            ),
            _ => None,
        }
    }
    let bodies = list(v.get("body_names").or_else(|| v.get("bodies")), &[',', ';', '\n'])
        .filter(|b| !b.is_empty());
    let notes = list(v.get("notes"), &[';', '|', '\n']).unwrap_or_default();
    let scale = parse_scale(v.get("scale"))?;
    Ok(Opts {
        bodies,
        hidden: v.get("hidden").and_then(Value::as_bool).unwrap_or(true),
        tolerance: v.get("tolerance").and_then(Value::as_f64).unwrap_or(0.15),
        title: v.get("title").and_then(Value::as_str).unwrap_or("").to_string(),
        dwg: v.get("dwg_no").and_then(Value::as_str).unwrap_or("").to_string(),
        out: output_name(v.get("output").and_then(Value::as_str)),
        inline: v.get("inline").and_then(Value::as_bool).unwrap_or(false),
        scale,
        notes,
        date: read_date(v.get("date"))?,
    })
}

// ---------------------------------------------------------------- meshes
struct Mesh { pos: Vec<P3>, tris: Vec<[usize; 3]> }

fn mesh_of(body: &str, tol: f64) -> Result<Mesh, PluginError> {
    let m = call("model.mesh", &serde_json::json!({ "body": body, "tolerance": tol }))?;
    let pos = m
        .get("positions")
        .and_then(Value::as_array)
        .ok_or_else(|| PluginError::Trap(format!("model.mesh({body}) gave no positions")))?;
    let mut pts = Vec::with_capacity(pos.len());
    for p in pos {
        let a = p.as_array().ok_or_else(|| PluginError::Trap("bad position".into()))?;
        pts.push([
            a.first().and_then(Value::as_f64).unwrap_or(0.0),
            a.get(1).and_then(Value::as_f64).unwrap_or(0.0),
            a.get(2).and_then(Value::as_f64).unwrap_or(0.0),
        ]);
    }
    let tris_json = m
        .get("triangles")
        .and_then(Value::as_array)
        .ok_or_else(|| PluginError::Trap(format!("model.mesh({body}) gave no triangles")))?;
    let mut tris = Vec::with_capacity(tris_json.len());
    for t in tris_json {
        let ok = t.as_array().filter(|a| a.len() >= 3);
        if let Some(a) = ok {
            let i0 = a[0].as_u64().unwrap_or(0) as usize;
            let i1 = a[1].as_u64().unwrap_or(0) as usize;
            let i2 = a[2].as_u64().unwrap_or(0) as usize;
            if i0 < pts.len() && i1 < pts.len() && i2 < pts.len() {
                tris.push([i0, i1, i2]);
            }
        }
    }
    Ok(Mesh { pos: pts, tris })
}

// feature edges: crease, boundary or silhouette
fn feature_edges(m: &Mesh, dir: P3, crease_deg: f64) -> Vec<(usize, usize)> {
    let mut map: HashMap<(usize, usize), (P3, P3, u8)> = HashMap::new();
    for t in &m.tris {
        let n = norm(cross(sub(m.pos[t[1]], m.pos[t[0]]), sub(m.pos[t[2]], m.pos[t[0]])));
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let key = if a < b { (a, b) } else { (b, a) };
            let e = map.entry(key).or_insert((n, n, 0));
            if e.2 == 0 { e.0 = n; } else { e.1 = n; }
            e.2 = e.2.saturating_add(1);
        }
    }
    let cos_lim = crease_deg.to_radians().cos();
    let mut out = Vec::new();
    for (key, (n1, n2, count)) in map {
        let keep = if count < 2 {
            true
        } else if dot(n1, n2) < cos_lim {
            true
        } else {
            (dot(n1, dir) > 0.0) != (dot(n2, dir) > 0.0)
        };
        if keep { out.push(key); }
    }
    out
}

// ---------------------------------------------------------------- svg helpers
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn f2(v: f64) -> String { format!("{:.2}", v) }
fn term(v: f64) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn text(x: f64, y: f64, size: f64, s: &str, anchor: &str, bold: bool) -> String {
    format!(
        "<text x=\"{}\" y=\"{}\" font-family=\"Helvetica,Arial,sans-serif\" font-size=\"{}\" \
         text-anchor=\"{}\" font-weight=\"{}\">{}</text>\n",
        f2(x), f2(y), f2(size), anchor, if bold { "bold" } else { "normal" }, esc(s)
    )
}

struct Sheet {
    svg: String,
    visible: usize,
    hidden: usize,
    scale_label: String,
    fits: bool,
}

#[allow(clippy::too_many_arguments)]
fn build_sheet(
    meshes: &[(String, Mesh)],
    bbox: (P3, P3),     // model bbox min/max (mm)
    unit: &str,
    unit_mm: f64,       // mm per design unit
    params: &[(String, String)],
    masses: &HashMap<String, f64>,
    vols: &HashMap<String, f64>,
    opts: &Opts,
    design: &str,
    date: &str,
) -> Sheet {
    let (right, up, dir) = basis();
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (_, m) in meshes {
        for p in &m.pos {
            let (sx, sy, _) = project(*p, right, up, dir);
            x0 = x0.min(sx); x1 = x1.max(sx);
            y0 = y0.min(sy); y1 = y1.max(sy);
        }
    }
    if !x0.is_finite() { x0 = 0.0; x1 = 1.0; y0 = 0.0; y1 = 1.0; }

    const VIEW: (f64, f64, f64, f64) = (16.0, 26.0, 300.0, 210.0); // x0, y0(top), x1, y1(bottom)
    let view = VIEW;
    let vw = view.2 - view.0;
    let vh = view.3 - view.1;
    let w = (x1 - x0).max(1e-6);
    let h = (y1 - y0).max(1e-6);

    // drafting scale, never larger than 1:1 unless asked
    let mut ratio = opts.scale.unwrap_or(0.0);
    if ratio <= 0.0 {
        ratio = 100.0;
        for r in [1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 10.0, 20.0, 50.0, 100.0] {
            if w / r <= vw - 30.0 && h / r <= vh - 14.0 { ratio = r; break; }
        }
    }
    let s = 1.0 / ratio; // page mm per model mm
    let fits = w * s <= vw && h * s <= vh;

    let ox = view.0 + (vw - w * s) / 2.0;
    let oy = view.1 + (vh - h * s) / 2.0;
    let px = |a: f64| ox + (a - x0) * s;
    let py = |b: f64| oy + (y1 - b) * s;
    let ratio_txt = if ratio >= 1.0 {
        if (ratio - 1.0).abs() < 1e-9 { "1:1".to_string() } else { format!("1:{}", term(ratio)) }
    } else {
        format!("{}:1", term(1.0 / ratio))
    };

    // ---- depth buffer
    let bw = 1500usize;
    let bh = ((bw as f64) * h / w).clamp(200.0, 1400.0) as usize;
    let kk = (bw as f64 / w).min(bh as f64 / h);
    let mut depth = vec![f64::NEG_INFINITY; bw * bh];
    for (_, m) in meshes {
        let pv: Vec<(f64, f64, f64)> = m.pos.iter().map(|p| project(*p, right, up, dir)).collect();
        for t in &m.tris {
            let a = pv[t[0]];
            let b = pv[t[1]];
            let c = pv[t[2]];
            let ax = (a.0 - x0) * kk; let ay = (a.1 - y0) * kk;
            let bx = (b.0 - x0) * kk; let by = (b.1 - y0) * kk;
            let cx = (c.0 - x0) * kk; let cy = (c.1 - y0) * kk;
            let minx = ax.min(bx).min(cx).floor().max(0.0) as usize;
            let maxx = ax.max(bx).max(cx).ceil().min(bw as f64 - 1.0) as usize;
            let miny = ay.min(by).min(cy).floor().max(0.0) as usize;
            let maxy = ay.max(by).max(cy).ceil().min(bh as f64 - 1.0) as usize;
            if maxx < minx || maxy < miny { continue; }
            let den = (by - cy) * (ax - cx) + (cx - bx) * (ay - cy);
            if den.abs() < 1e-12 { continue; }
            for yy in miny..=maxy {
                for xx in minx..=maxx {
                    let l1 = ((by - cy) * (xx as f64 - cx) + (cx - bx) * (yy as f64 - cy)) / den;
                    let l2 = ((cy - ay) * (xx as f64 - cx) + (ax - cx) * (yy as f64 - cy)) / den;
                    let l3 = 1.0 - l1 - l2;
                    if l1 < -1e-9 || l2 < -1e-9 || l3 < -1e-9 { continue; }
                    let d = l1 * a.2 + l2 * b.2 + l3 * c.2;
                    let i = yy * bw + xx;
                    if d > depth[i] { depth[i] = d; }
                }
            }
        }
    }

    // ---- line art with hidden-line removal
    let eps = (opts.tolerance * 3.0).max(0.6);
    let mut vis_paths: Vec<String> = Vec::new();
    let mut hid_paths: Vec<String> = Vec::new();
    let mut nvis = 0usize;
    let mut nhid = 0usize;

    for (_, m) in meshes {
        if control::cancelled() { break; }
        let edges = feature_edges(m, dir, 20.0);
        let pv: Vec<(f64, f64, f64)> = m.pos.iter().map(|p| project(*p, right, up, dir)).collect();
        for (i, j) in edges {
            let a = pv[i];
            let b = pv[j];
            let (ax, ay) = (px(a.0), py(a.1));
            let (bx, by) = (px(b.0), py(b.1));
            let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
            if len < 0.25 { continue; }
            let steps = ((len / 0.7).ceil() as usize).clamp(2, 400);
            let mut run_start: Option<(f64, f64, bool)> = None;
            let mut run_last = (0.0, 0.0, false);
            let mut run_state = false;
            for k in 0..=steps {
                let t = k as f64 / steps as f64;
                let sxm = a.0 + (b.0 - a.0) * t;
                let sym = a.1 + (b.1 - a.1) * t;
                let dep = a.2 + (b.2 - a.2) * t;
                let gx = ((sxm - x0) * kk) as isize;
                let gy = ((sym - y0) * kk) as isize;
                let mut visible = true;
                if gx >= 0 && gy >= 0 && (gx as usize) < bw && (gy as usize) < bh {
                    let d = depth[gy as usize * bw + gx as usize];
                    visible = !d.is_finite() || dep >= d - eps;
                }
                let x = ax + (bx - ax) * t;
                let y = ay + (by - ay) * t;
                if k == 0 { run_start = Some((x, y, visible)); run_state = visible; }
                else if visible != run_state {
                    if let Some(st) = run_start {
                        emit(&mut vis_paths, &mut hid_paths, &mut nvis, &mut nhid, st.2, st.0, st.1, run_last.0, run_last.1);
                    }
                    run_start = Some((x, y, visible));
                    run_state = visible;
                }
                run_last = (x, y, visible);
            }
            if let Some(st) = run_start {
                emit(&mut vis_paths, &mut hid_paths, &mut nvis, &mut nhid, st.2, st.0, st.1, run_last.0, run_last.1);
            }
        }
    }

    let u = unit_mm;
    let to_unit = |mm: f64| mm / u;
    let fmt = |mm: f64, dec: usize| format!("{:.*}", dec, to_unit(mm));

    let mut svg = String::with_capacity(260_000);
    svg.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"431.8mm\" height=\"279.4mm\" viewBox=\"0 0 431.8 279.4\">\n");
    svg.push_str("<rect x=\"0\" y=\"0\" width=\"431.8\" height=\"279.4\" fill=\"#ffffff\"/>\n");
    svg.push_str("<g fill=\"none\" stroke=\"#000000\" stroke-width=\"0.6\"><rect x=\"10\" y=\"10\" width=\"411.8\" height=\"259.4\"/><rect x=\"13\" y=\"13\" width=\"405.8\" height=\"253.4\" stroke-width=\"0.25\"/></g>\n");

    // ---- view
    svg.push_str("<g fill=\"none\" stroke=\"#000000\" stroke-width=\"0.30\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\n");
    for p in &vis_paths { svg.push_str(p); }
    svg.push_str("</g>\n");
    if opts.hidden && !hid_paths.is_empty() {
        svg.push_str("<g fill=\"none\" stroke=\"#8a8a8a\" stroke-width=\"0.18\" stroke-dasharray=\"1.6 1.2\" stroke-linecap=\"round\">\n");
        for p in &hid_paths { svg.push_str(p); }
        svg.push_str("</g>\n");
    }

    // ---- overall dimensions
    let (minx, maxx) = (bbox.0[0], bbox.1[0]);
    let (miny, maxy) = (bbox.0[1], bbox.1[1]);
    let (minz, maxz) = (bbox.0[2], bbox.1[2]);
    let mut dims = String::new();
    dims.push_str("<g stroke=\"#000000\" stroke-width=\"0.2\" fill=\"#000000\" font-family=\"Helvetica,Arial,sans-serif\">\n");
    let top_page = py(y1);
    let bot_page = py(y0);
    let dy = view.3 + 6.0;
    let (dx0, dx1) = (px(x0), px(x1));
    dims.push_str(&format!("<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke-width=\"0.15\"/>\n", f2(dx0), f2(bot_page - 1.0), f2(dx0), f2(dy + 2.0)));
    dims.push_str(&format!("<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke-width=\"0.15\"/>\n", f2(dx1), f2(bot_page - 1.0), f2(dx1), f2(dy + 2.0)));
    dims.push_str(&format!("<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>\n", f2(dx0), f2(dy), f2(dx1), f2(dy)));
    arrow(&mut dims, dx0, dy, -1.0);
    arrow(&mut dims, dx1, dy, 1.0);
    let wl = fmt(maxx - minx, 3);
    dims.push_str(&text((dx0 + dx1) / 2.0, dy - 1.4, 3.2, &wl, "middle", false));
    // height, left of the view
    let hx = (px(x0) - 8.0).max(view.0 + 5.0);
    dims.push_str(&format!("<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke-width=\"0.15\"/>\n", f2(px(x0) - 1.0), f2(top_page), f2(hx - 1.5), f2(top_page)));
    dims.push_str(&format!("<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke-width=\"0.15\"/>\n", f2(px(x0) - 1.0), f2(bot_page), f2(hx - 1.5), f2(bot_page)));
    dims.push_str(&format!("<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>\n", f2(hx), f2(top_page), f2(hx), f2(bot_page)));
    arrow_v(&mut dims, hx, top_page, -1.0);
    arrow_v(&mut dims, hx, bot_page, 1.0);
    let hl = fmt(maxy - miny, 3);
    dims.push_str(&format!(
        "<text x=\"{}\" y=\"{}\" font-size=\"3.2\" text-anchor=\"middle\" transform=\"rotate(-90 {} {})\">{}</text>\n",
        f2(hx - 1.6), f2((top_page + bot_page) / 2.0), f2(hx - 1.6), f2((top_page + bot_page) / 2.0), esc(&hl)
    ));
    dims.push_str("</g>\n");
    svg.push_str(&dims);

    // view label
    svg.push_str(&text((view.0 + view.2) / 2.0, view.3 + 15.0, 4.6, "ISOMETRIC VIEW - ASSEMBLY", "middle", true));
    svg.push_str(&text((view.0 + view.2) / 2.0, view.3 + 20.0, 3.0,
        &format!("SCALE {}   -   DIMENSIONS IN {}", ratio_txt, if unit == "in" { "INCHES" } else { unit }), "middle", false));

    // ---- right column
    let cx = 306.0f64;
    let mut y = 26.0f64;
    svg.push_str(&text(cx, y, 4.0, "BILL OF MATERIALS", "start", true));
    y += 5.2;
    svg.push_str(&text(cx, y, 2.6, "ITEM  BODY                  VOLUME        MASS", "start", true));
    y += 1.0;
    svg.push_str(&format!("<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" stroke=\"#000000\" stroke-width=\"0.2\"/>\n", f2(cx), f2(y), f2(420.8), f2(y)));
    y += 3.8;
    for (i, (name, _)) in meshes.iter().enumerate() {
        let v = vols.get(name).copied().unwrap_or(0.0);
        let vt = v / (u * u * u);
        let m = masses.get(name).copied().unwrap_or(0.0);
        svg.push_str(&text(cx, y, 2.7,
            &format!("{}.  {:<20} {:>9.4} {}3 {:>6.1} g", i + 1, shorten(name, 20), vt,
                     if unit == "in" { "in" } else { unit }, m),
            "start", false));
        y += 3.7;
    }
    y += 4.0;
    svg.push_str(&text(cx, y, 3.6, "PARAMETERS", "start", true));
    y += 4.6;
    for (n, v) in params.iter() {
        if y > 152.0 { break; }
        svg.push_str(&text(cx, y, 2.6, &format!("{:<21} {}", shorten(n, 21), v), "start", false));
        y += 3.25;
    }

    // ---- notes
    let mut ny = 168.0f64;
    svg.push_str(&text(cx, ny, 3.6, "NOTES", "start", true));
    ny += 4.6;
    for n in &opts.notes {
        if ny > 214.0 { break; }
        svg.push_str(&text(cx, ny, 2.5, n, "start", false));
        ny += 3.3;
    }

    // ---- title block (bottom right)
    let tb = (262.0f64, 230.0f64, 421.8f64, 269.4f64);
    svg.push_str(&format!("<g fill=\"none\" stroke=\"#000000\" stroke-width=\"0.4\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></g>\n",
        f2(tb.0), f2(tb.1), f2(tb.2 - tb.0), f2(tb.3 - tb.1)));
    svg.push_str(&format!("<g stroke=\"#000000\" stroke-width=\"0.2\"><line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/><line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/></g>\n",
        f2(tb.0), f2(tb.1 + 22.0), f2(tb.2), f2(tb.1 + 22.0), f2(tb.0 + 118.0), f2(tb.1), f2(tb.0 + 118.0), f2(tb.3)));
    svg.push_str(&text(tb.0 + 2.0, tb.1 + 4.6, 2.4, "TITLE", "start", false));
    svg.push_str(&text(tb.0 + 2.0, tb.1 + 12.0, 6.0, &shorten(if opts.title.is_empty() { design } else { &opts.title }, 40), "start", true));
    svg.push_str(&text(tb.0 + 2.0, tb.1 + 17.5, 2.8, "ASSEMBLY - ISOMETRIC MANUFACTURE SHEET", "start", false));
    svg.push_str(&text(tb.0 + 121.0, tb.1 + 4.6, 2.4, "DWG NO.", "start", false));
    svg.push_str(&text(tb.0 + 121.0, tb.1 + 11.5, 4.6, if opts.dwg.is_empty() { "-" } else { &opts.dwg }, "start", true));
    svg.push_str(&text(tb.0 + 121.0, tb.1 + 17.5, 2.4, &format!("REV  -"), "start", false));
    svg.push_str(&text(tb.0 + 2.0, tb.1 + 26.5, 2.4, &format!("SCALE {:<7} UNITS {:<5} DATE {}", ratio_txt, if unit == "in" { "INCH" } else { unit }, date), "start", false));
    svg.push_str(&text(tb.0 + 2.0, tb.1 + 31.0, 2.4, &format!("ENVELOPE {} x {} x {} {}", fmt(maxx - minx, 3), fmt(maxy - miny, 3), fmt(maxz - minz, 3), if unit == "in" { "IN" } else { unit }), "start", false));
    svg.push_str(&text(tb.0 + 2.0, tb.1 + 34.0, 2.2, "GENERATED BY SolidCraft - iso-sheet plugin", "start", false));
    svg.push_str(&text(tb.2 - 2.0, tb.1 + 34.0, 2.8, "SHEET 1 OF 1", "end", true));

    svg.push_str("</svg>\n");

    Sheet { svg, visible: nvis, hidden: nhid, scale_label: ratio_txt, fits }
}

fn shorten(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n.saturating_sub(1)).collect::<String>() + "~" }
}

fn emit(vis: &mut Vec<String>, hid: &mut Vec<String>, nv: &mut usize, nh: &mut usize,
        visible: bool, x0: f64, y0: f64, x1: f64, y1: f64) {
    let seg = if visible {
        *nv += 1;
        format!("<path d=\"M{} {}L{} {}\"/>\n", f2(x0), f2(y0), f2(x1), f2(y1))
    } else {
        *nh += 1;
        format!("<path d=\"M{} {}L{} {}\"/>\n", f2(x0), f2(y0), f2(x1), f2(y1))
    };
    if visible { vis.push(seg) } else { hid.push(seg) }
}

fn arrow(out: &mut String, x: f64, y: f64, dir: f64) {
    let a = 1.6;
    out.push_str(&format!(
        "<path d=\"M{} {}L{} {}L{} {}Z\"/>\n",
        f2(x), f2(y), f2(x + dir * a), f2(y - 0.6), f2(x + dir * a), f2(y + 0.6)
    ));
}
fn arrow_v(out: &mut String, x: f64, y: f64, dir: f64) {
    let a = 1.6;
    out.push_str(&format!(
        "<path d=\"M{} {}L{} {}L{} {}Z\"/>\n",
        f2(x), f2(y), f2(x - 0.6), f2(y + dir * a), f2(x + 0.6), f2(y + dir * a)
    ));
}

// ---------------------------------------------------------------- entry
fn sheet(params: &str) -> Result<String, PluginError> {
    let opts = read_opts(params)?;
    control::progress(0.05, "reading design");
    let doc = call("document.inspect", &serde_json::json!({
        "measure": false, "timeline": false, "bodies": "names"
    }))?;
    let design = doc.get("name").and_then(Value::as_str).unwrap_or("Design").to_string();
    let unit = doc.get("units").and_then(Value::as_str).unwrap_or("mm").to_string();
    let unit_mm = match unit.as_str() { "in" => 25.4, "cm" => 10.0, "m" => 1000.0, _ => 1.0 };

    let available: Vec<String> = doc
        .get("bodies")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|b| b.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut names: Vec<String> = match &opts.bodies {
        Some(requested) => requested
            .iter()
            .map(|want| {
                available
                    .iter()
                    .find(|name| name.eq_ignore_ascii_case(want))
                    .cloned()
                    .ok_or_else(|| {
                        let hint = if available.is_empty() {
                            "the design has no bodies".to_string()
                        } else {
                            format!("available: {}", available.join(", "))
                        };
                        invalid("body_names", &format!("unknown body '{want}'; {hint}"))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => available.clone(),
    };
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    if names.is_empty() {
        return Err(invalid("body_names", "the design has no bodies to draw"));
    }

    control::progress(0.15, "measuring");
    let mut vols: HashMap<String, f64> = HashMap::new();
    let mut masses: HashMap<String, f64> = HashMap::new();
    let mut bmin = [f64::INFINITY; 3];
    let mut bmax = [f64::NEG_INFINITY; 3];
    let meas = call("inspect.measure", &serde_json::json!({}))?;
    if let Some(list) = meas.get("bodies").and_then(Value::as_array) {
        for b in list {
            let n = b.get("name").and_then(Value::as_str).unwrap_or("");
            if !names.iter().any(|x| x == n) { continue; }
            if let Some(v) = b.get("volume_mm3").and_then(Value::as_f64) { vols.insert(n.to_string(), v); }
            if let Some(m) = b.get("mass_g").and_then(Value::as_f64) { masses.insert(n.to_string(), m); }
            if let Some(bb) = b.get("bbox") {
                for k in 0..3 {
                    if let Some(v) = bb.get("min").and_then(|m| m.get(k)).and_then(Value::as_f64) { bmin[k] = bmin[k].min(v); }
                    if let Some(v) = bb.get("max").and_then(|m| m.get(k)).and_then(Value::as_f64) { bmax[k] = bmax[k].max(v); }
                }
            }
        }
    }
    if !bmin[0].is_finite() { bmin = [0.0; 3]; bmax = [1.0; 3]; }

    // parameters (name -> display value)
    let mut plist: Vec<(String, String)> = Vec::new();
    if let Some(ps) = doc.get("params").and_then(Value::as_array) {
        for p in ps {
            let n = p.get("name").and_then(Value::as_str).unwrap_or("");
            let v = p.get("value").and_then(Value::as_f64);
            let u = p.get("unit").and_then(Value::as_str).unwrap_or("");
            let generated = n.len() > 1 && n.starts_with('d') && n[1..].bytes().all(|b| b.is_ascii_digit());
            if n.is_empty() || generated { continue; }
            if let Some(v) = v {
                plist.push((n.to_string(),
                    if u == "deg" { format!("{:.2} deg", v * 180.0 / std::f64::consts::PI) }
                    else { format!("{:.4} {}", v / unit_mm, if unit == "in" { "in" } else { unit.as_str() }) }));
            }
        }
    }

    control::progress(0.25, "reading meshes");
    let mut meshes: Vec<(String, Mesh)> = Vec::new();
    for (i, n) in names.iter().enumerate() {
        if control::cancelled() { return Err(PluginError::Cancelled); }
        control::progress(0.25 + 0.5 * (i as f32 / names.len() as f32), &format!("mesh {n}"));
        let m = mesh_of(n, opts.tolerance)?;
        if !m.tris.is_empty() { meshes.push((n.clone(), m)); }
    }
    if meshes.is_empty() {
        return Err(invalid("bodies", "no triangulated geometry for the chosen bodies"));
    }

    control::progress(0.8, "drawing");
    let date = opts.date.clone();
    let sheet = build_sheet(&meshes, (bmin, bmax), &unit, unit_mm, &plist, &masses, &vols, &opts, &design, &date);

    let mut written: Option<String> = None;
    let mut write_error: Option<String> = None;
    if let Some(path) = &opts.out {
        let bytes = sheet.svg.as_bytes().to_vec();
        match files::write_file("output", path, &bytes) {
            Ok(_) => { written = Some(path.clone()); }
            Err(e) => {
                let text = format!("{e:?}");
                host_log(Level::Warn, &format!("write-file refused: {text}"));
                write_error = Some(text);
            }
        }
    }

    control::progress(1.0, "done");
    let mut out = serde_json::json!({
        "ok": true,
        "design": design,
        "units": unit,
        "bodies": meshes.iter().map(|(n, m)| serde_json::json!({
            "name": n, "triangles": m.tris.len(), "vertices": m.pos.len()
        })).collect::<Vec<_>>(),
        "visible_segments": sheet.visible,
        "hidden_segments": sheet.hidden,
        "scale": sheet.scale_label,
        "date": date,
        "available_bodies": available,
        "svg_bytes": sheet.svg.len(),
        "path": written,
        "write_error": write_error,
        "envelope": {
            "x": (bmax[0] - bmin[0]) / unit_mm,
            "y": (bmax[1] - bmin[1]) / unit_mm,
            "z": (bmax[2] - bmin[2]) / unit_mm,
            "unit": unit,
        },
    });
    if !sheet.fits {
        out["scale_note"] = Value::String(
            "The drawing does not fit the sheet at the selected scale; choose a larger denominator (for example 1:10)."
                .into(),
        );
    }
    if written.is_none() {
        out["hint"] = Value::String(
            "No output folder is granted to this plugin, so the sheet is returned here instead. Grant the plugin a folder (permission \"output\") in Preferences, then run it again to write the SVG."
                .into());
    }
    if opts.inline || written.is_none() {
        if sheet.svg.len() < 800_000 {
            out["svg"] = Value::String(sheet.svg.clone());
        } else {
            out["svg_note"] = Value::String("sheet exceeds the inline budget; pass output to write it".into());
        }
    }
    Ok(out.to_string())
}

fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as f64;
    let yoe = ((doe - doe / 1460.0 + doe / 36_524.0 - doe / 146_096.0) / 365.0).floor();
    let y = yoe as i64 + era * 400;
    let doy = doe - (365.0 * yoe + (yoe / 4.0).floor() - (yoe / 100.0).floor());
    let mp = ((5.0 * doy + 2.0) / 153.0).floor();
    let d = doy - (153.0 * mp + 2.0) / 5.0 + 1.0;
    let m = if mp < 10.0 { mp + 3.0 } else { mp - 9.0 };
    let y = if m <= 2.0 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m as i64, d as i64)
}

impl Guest for IsoSheet {
    fn execute(entry: String, params: String) -> Result<String, PluginError> {
        match entry.as_str() {
            "sheet" => sheet(&params),
            other => Err(invalid("entry", &format!("unknown entry '{other}'"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn dates_are_strict_iso() {
        assert!(valid_date("2026-11-14"));
        assert!(valid_date("2024-02-29"));
        assert!(!valid_date("2023-02-29"));
        assert!(!valid_date("2026-13-01"));
        assert!(!valid_date("2026-11-31"));
        assert!(!valid_date("11/14/2026"));
        assert!(!valid_date("2026-1-4"));
    }

    #[test]
    fn scales_accept_the_dropdown_and_legacy_forms() {
        assert_eq!(parse_scale(Some(&json!(null))).unwrap(), None);
        assert_eq!(parse_scale(Some(&json!("Auto"))).unwrap(), None);
        assert_eq!(parse_scale(Some(&json!(""))).unwrap(), None);
        assert_eq!(parse_scale(Some(&json!("1:2"))).unwrap(), Some(2.0));
        assert_eq!(parse_scale(Some(&json!("2"))).unwrap(), Some(2.0));
        assert_eq!(parse_scale(Some(&json!("2:1"))).unwrap(), Some(0.5));
        assert!(parse_scale(Some(&json!("0"))).is_err());
        assert!(parse_scale(Some(&json!("bogus"))).is_err());
    }

    #[test]
    fn output_paths_get_a_file_name() {
        assert_eq!(output_name(Some("")), None);
        assert_eq!(output_name(Some("part")), Some("part.svg".to_string()));
        assert_eq!(output_name(Some("part.SVG")), Some("part.SVG".to_string()));
        assert_eq!(output_name(Some("sheets/")), Some("sheets/iso-sheet.svg".to_string()));
        assert_eq!(output_name(Some("sheets\\")), Some("sheets\\iso-sheet.svg".to_string()));
    }
}


