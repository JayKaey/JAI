//! The 3D studio's renderer: an egui paint callback with its own shaders, drawing the model into
//! an offscreen multisampled framebuffer and laying the result over the block.
//!
//! Two looks. Realistic: physically based shading (base colour, metallic, roughness, normal,
//! occlusion and emissive maps; Cook-Torrance GGX) under a procedural studio (soft boxes, a rim
//! light in the theme's colour, ACES tone mapping), standing on a soft floor with a contact
//! shadow. Hologram: a depth pre-pass, then additive fresnel and scan lines in the theme's colour
//! over a glowing platform, with the mesh's lines on top. Planets have no floor; the Earth's city
//! lights shine on the night side only and an atmosphere glows at the limb.
//!
//! The GL objects live in one static (eframe's viewports share one GL context). The panel asks
//! for `release` when the studio closes; the next frame of any HUD window frees them. A GPU
//! without OpenGL 3 / GLES 3 (or a shader that does not compile) sets `Status::Unsupported` and
//! the panel draws the model's edges with egui instead.

use crate::mesh_io::{math, AlphaMode, Material};
use crate::model3d::{Look, Scene};
use eframe::egui;
use eframe::egui_glow;
use eframe::glow::{self, HasContext};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tracing::{info, warn};

/// What one frame shows.
#[derive(Clone)]
pub struct Frame {
    pub scene: Arc<Scene>,
    /// Model space → world (normalised, turned, scaled).
    pub model: math::M4,
    pub view: math::M4,
    pub proj: math::M4,
    pub eye: [f32; 3],
    /// The camera's turn about the vertical (the studio's lights follow the camera).
    pub cam_yaw: f32,
    pub look: Look,
    pub wireframe: bool,
    /// 0 together … 1 pulled apart.
    pub explode: f32,
    /// The theme's colour (sRGB, 0–1).
    pub accent: [f32; 3],
    /// Seconds (scan lines, pulses).
    pub time: f32,
    /// The floor's height in the world and the model's footprint radius on it.
    pub ground_y: f32,
    pub footprint: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Not tried yet (no frame drawn).
    Unknown,
    Ready,
    /// The panel draws the egui fallback.
    Unsupported,
}

static STATUS: AtomicU8 = AtomicU8::new(0);
static HOLDS: AtomicBool = AtomicBool::new(false);
static RELEASE: AtomicBool = AtomicBool::new(false);
static RENDERER: LazyLock<Mutex<Option<Renderer>>> = LazyLock::new(|| Mutex::new(None));
static REASON: Mutex<String> = Mutex::new(String::new());

pub fn status() -> Status {
    match STATUS.load(Ordering::Acquire) {
        1 => Status::Ready,
        2 => Status::Unsupported,
        _ => Status::Unknown,
    }
}

/// Why the GPU path is off (for the panel's small print).
pub fn unsupported_reason() -> Option<String> {
    (status() == Status::Unsupported).then(|| REASON.lock().map(|r| r.clone()).unwrap_or_default())
}

/// GPU memory is held (meshes, textures, the framebuffer).
pub fn holds_gpu_memory() -> bool {
    HOLDS.load(Ordering::Acquire)
}

/// Frees the GPU memory on the next frame of any HUD window.
pub fn release() {
    RELEASE.store(true, Ordering::Release);
}

fn unsupported(why: String) {
    warn!("[3D] The GPU renderer is off: {why}");
    if let Ok(mut r) = REASON.lock() {
        *r = why;
    }
    STATUS.store(2, Ordering::Release);
}

/// The paint callback drawing `frame` into `rect`.
pub fn callback(rect: egui::Rect, frame: Frame) -> egui::PaintCallback {
    let cb = egui_glow::CallbackFn::new(move |info, painter| {
        let gl = painter.gl();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { paint(gl, &info, &frame) }));
        if outcome.is_err() {
            unsupported("the renderer panicked".into());
            if let Ok(mut r) = RENDERER.lock() {
                if let Some(r) = r.take() {
                    unsafe { r.destroy(gl) };
                }
            }
            HOLDS.store(false, Ordering::Release);
        }
    });
    egui::PaintCallback { rect, callback: Arc::new(cb) }
}

/// A callback that only frees the GPU memory (drawn when the studio is closed).
pub fn release_callback(rect: egui::Rect) -> egui::PaintCallback {
    let cb = egui_glow::CallbackFn::new(move |_info, painter| {
        RELEASE.store(false, Ordering::Release);
        if let Ok(mut r) = RENDERER.lock() {
            if let Some(r) = r.take() {
                unsafe { r.destroy(painter.gl()) };
                info!("[3D] Freed the studio's GPU memory.");
            }
        }
        HOLDS.store(false, Ordering::Release);
    });
    egui::PaintCallback { rect, callback: Arc::new(cb) }
}

unsafe fn paint(gl: &glow::Context, info: &egui::PaintCallbackInfo, frame: &Frame) {
    let Ok(mut slot) = RENDERER.lock() else { return };
    if RELEASE.swap(false, Ordering::AcqRel) {
        if let Some(r) = slot.take() {
            r.destroy(gl);
        }
        HOLDS.store(false, Ordering::Release);
    }
    if status() == Status::Unsupported {
        return;
    }
    if slot.is_none() {
        match Renderer::new(gl) {
            Ok(r) => {
                *slot = Some(r);
                STATUS.store(1, Ordering::Release);
            }
            Err(e) => {
                unsupported(e);
                return;
            }
        }
    }
    if let Some(r) = slot.as_mut() {
        HOLDS.store(true, Ordering::Release);
        if let Err(e) = r.draw(gl, info, frame) {
            warn!("[3D] Frame skipped: {e}");
        }
        restore(gl, r.es);
    }
}

/// What egui does not set again itself after a callback.
unsafe fn restore(gl: &glow::Context, es: bool) {
    gl.depth_mask(true);
    gl.disable(glow::DEPTH_TEST);
    gl.disable(glow::CULL_FACE);
    gl.disable(glow::POLYGON_OFFSET_FILL);
    if !es {
        gl.disable(glow::POLYGON_OFFSET_LINE);
        gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
    }
    gl.color_mask(true, true, true, true);
    for unit in 1..6 {
        gl.active_texture(glow::TEXTURE0 + unit);
        gl.bind_texture(glow::TEXTURE_2D, None);
    }
    gl.active_texture(glow::TEXTURE0);
    gl.bind_vertex_array(None);
    gl.bind_buffer(glow::ARRAY_BUFFER, None);
    gl.use_program(None);
}

// ------------------------------------------------------------------------------ shaders ----

const VS_MESH: &str = r#"
in vec3 a_pos;
in vec3 a_nrm;
in vec2 a_uv;
in vec4 a_tan;
in vec3 a_col;
uniform mat4 u_viewproj;
uniform mat4 u_model;
uniform mat4 u_normal;
uniform mat4 u_tri;
out vec3 v_world;
out vec3 v_nrm;
out vec3 v_onrm;
out vec2 v_uv;
out vec4 v_tan;
out vec3 v_obj;
out vec3 v_col;
void main() {
    vec4 w = u_model * vec4(a_pos, 1.0);
    v_world = w.xyz;
    v_nrm = mat3(u_normal) * a_nrm;
    v_onrm = a_nrm;
    v_tan = vec4(mat3(u_model) * a_tan.xyz, a_tan.w);
    v_uv = a_uv;
    v_obj = (u_tri * vec4(a_pos, 1.0)).xyz;
    v_col = a_col;
    gl_Position = u_viewproj * w;
}
"#;

const FS_PBR: &str = r#"
in vec3 v_world;
in vec3 v_nrm;
in vec3 v_onrm;
in vec2 v_uv;
in vec4 v_tan;
in vec3 v_obj;
in vec3 v_col;
out vec4 o_color;
uniform vec4 u_base;
uniform vec3 u_emissive;
uniform float u_metallic;
uniform float u_roughness;
uniform float u_nscale;
uniform float u_cutoff;
uniform float u_tile;
uniform int u_flags;
uniform sampler2D t_base;
uniform sampler2D t_mr;
uniform sampler2D t_nrm;
uniform sampler2D t_emis;
uniform sampler2D t_occ;
uniform mat4 u_normal;
uniform mat4 u_envrot;
uniform vec3 u_eye;
uniform vec3 u_accent;
uniform vec3 u_key;
uniform vec3 u_fill;
uniform vec3 u_rim;
uniform float u_env;
uniform float u_keyk;

const float PI = 3.14159265;

vec3 lin(vec3 c) { return pow(max(c, vec3(0.0)), vec3(2.2)); }

vec3 studio(vec3 d, float r) {
    d = mat3(u_envrot) * d;
    float sharp = mix(80.0, 2.0, r);
    float k = mix(1.0, 0.32, r);
    vec3 col = mix(vec3(0.03, 0.033, 0.04), vec3(0.40, 0.42, 0.46), smoothstep(-0.25, 0.85, d.y));
    col += vec3(3.0) * k * pow(max(dot(d, normalize(vec3(0.15, 1.0, 0.35))), 0.0), sharp);
    col += vec3(2.3, 2.2, 2.05) * k * pow(max(dot(d, normalize(vec3(-1.0, 0.45, 0.7))), 0.0), sharp * 1.6);
    col += vec3(1.2, 1.3, 1.5) * k * pow(max(dot(d, normalize(vec3(1.0, 0.25, -0.35))), 0.0), sharp * 1.6);
    col += lin(u_accent) * 1.6 * k * pow(max(dot(d, normalize(vec3(0.0, 0.25, -1.0))), 0.0), sharp);
    return col;
}

vec2 env_brdf(float nv, float r) {
    const vec4 c0 = vec4(-1.0, -0.0275, -0.572, 0.022);
    const vec4 c1 = vec4(1.0, 0.0425, 1.04, -0.04);
    vec4 rr = r * c0 + c1;
    float a004 = min(rr.x * rr.x, exp2(-9.28 * nv)) * rr.x + rr.y;
    return vec2(-1.04, 1.04) * a004 + rr.zw;
}

vec3 light(vec3 n, vec3 v, vec3 l, vec3 radiance, vec3 albedo, float metal, float rough, vec3 f0) {
    float nl = dot(n, l);
    if (nl <= 0.0) return vec3(0.0);
    vec3 h = normalize(v + l);
    float nv = max(dot(n, v), 1e-4);
    float nh = max(dot(n, h), 0.0);
    float a = rough * rough;
    float a2 = a * a;
    float dd = nh * nh * (a2 - 1.0) + 1.0;
    float D = a2 / (PI * dd * dd);
    float k = (rough + 1.0) * (rough + 1.0) / 8.0;
    float G = (nv / (nv * (1.0 - k) + k)) * (nl / (nl * (1.0 - k) + k));
    vec3 F = f0 + (1.0 - f0) * pow(1.0 - max(dot(h, v), 0.0), 5.0);
    vec3 spec = D * G * F / (4.0 * nv * nl + 1e-4);
    vec3 kd = (1.0 - F) * (1.0 - metal);
    return (kd * albedo / PI + spec) * radiance * nl;
}

vec3 tri_weights(vec3 n) {
    vec3 w = pow(abs(n), vec3(4.0));
    return w / (w.x + w.y + w.z + 1e-5);
}

vec4 tri(sampler2D t, vec3 p, vec3 w) {
    return texture(t, p.zy) * w.x + texture(t, p.xz) * w.y + texture(t, p.xy) * w.z;
}

vec3 aces(vec3 x) {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0);
}

void main() {
    bool triplanar = (u_flags & 32) != 0;
    vec3 on = normalize(v_onrm);
    if (!gl_FrontFacing) on = -on;
    vec3 tw = tri_weights(on);
    vec3 tp = v_obj * u_tile;
    vec4 base = u_base;
    if ((u_flags & 1) != 0) {
        vec4 t = triplanar ? tri(t_base, tp, tw) : texture(t_base, v_uv);
        base *= vec4(lin(t.rgb), t.a);
    }
    if ((u_flags & 512) != 0) base.rgb *= v_col;
    if ((u_flags & 64) != 0 && base.a < u_cutoff) discard;
    float metal = u_metallic;
    float rough = u_roughness;
    float ao = 1.0;
    if ((u_flags & 2) != 0) {
        vec4 mr = triplanar ? tri(t_mr, tp, tw) : texture(t_mr, v_uv);
        rough *= mr.g;
        metal *= mr.b;
    }
    if ((u_flags & 16) != 0) {
        ao = triplanar ? tri(t_occ, tp, tw).r : texture(t_occ, v_uv).r;
    }
    rough = clamp(rough, 0.045, 1.0);
    metal = clamp(metal, 0.0, 1.0);
    vec3 n = normalize(v_nrm);
    if (!gl_FrontFacing) n = -n;
    if ((u_flags & 4) != 0) {
        if (triplanar) {
            vec3 tx = texture(t_nrm, tp.zy).xyz * 2.0 - 1.0;
            vec3 ty = texture(t_nrm, tp.xz).xyz * 2.0 - 1.0;
            vec3 tz = texture(t_nrm, tp.xy).xyz * 2.0 - 1.0;
            tx.y = -tx.y;
            ty.y = -ty.y;
            tz.y = -tz.y;
            tx.xy *= u_nscale;
            ty.xy *= u_nscale;
            tz.xy *= u_nscale;
            tx = vec3(tx.xy + on.zy, abs(tx.z) * on.x);
            ty = vec3(ty.xy + on.xz, abs(ty.z) * on.y);
            tz = vec3(tz.xy + on.xy, abs(tz.z) * on.z);
            vec3 pn = normalize(tx.zyx * tw.x + ty.xzy * tw.y + tz.xyz * tw.z);
            n = normalize(mat3(u_normal) * pn);
        } else {
            vec3 tn = texture(t_nrm, v_uv).xyz * 2.0 - 1.0;
            tn.xy *= u_nscale;
            mat3 tbn;
            if (abs(v_tan.w) > 0.5) {
                vec3 t = normalize(v_tan.xyz - n * dot(n, v_tan.xyz));
                vec3 b = cross(n, t) * v_tan.w;
                tbn = mat3(t, b, n);
            } else {
                vec3 dp1 = dFdx(v_world);
                vec3 dp2 = dFdy(v_world);
                vec2 du1 = dFdx(v_uv);
                vec2 du2 = dFdy(v_uv);
                vec3 p2 = cross(dp2, n);
                vec3 p1 = cross(n, dp1);
                vec3 t = p2 * du1.x + p1 * du2.x;
                vec3 b = p2 * du1.y + p1 * du2.y;
                float inv = inversesqrt(max(max(dot(t, t), dot(b, b)), 1e-20));
                tbn = mat3(t * inv, -b * inv, n);
            }
            n = normalize(tbn * tn);
        }
    }
    vec3 v = normalize(u_eye - v_world);
    vec3 albedo = base.rgb;
    vec3 f0 = mix(vec3(0.04), albedo, metal);
    vec3 col = vec3(0.0);
    col += light(n, v, u_key, vec3(2.8, 2.7, 2.55) * u_keyk, albedo, metal, rough, f0);
    col += light(n, v, u_fill, vec3(0.65, 0.72, 0.85) * u_env, albedo, metal, rough, f0);
    col += light(n, v, u_rim, lin(u_accent) * 2.4 * u_env, albedo, metal, rough, f0);
    float nv = max(dot(n, v), 1e-4);
    vec2 ab = env_brdf(nv, rough);
    vec3 spec_env = studio(reflect(-v, n), rough) * (f0 * ab.x + ab.y);
    vec3 diff_env = studio(n, 1.0) * albedo * (1.0 - metal) * 0.85;
    col += (spec_env + diff_env) * ao * u_env;
    vec3 emis = u_emissive;
    if ((u_flags & 8) != 0) {
        vec3 e = triplanar ? tri(t_emis, tp, tw).rgb : texture(t_emis, v_uv).rgb;
        emis *= lin(e);
    }
    if ((u_flags & 128) != 0) emis *= smoothstep(0.12, -0.22, dot(n, u_key));
    col += emis;
    col = pow(aces(col), vec3(1.0 / 2.2));
    float alpha = (u_flags & 256) != 0 ? clamp(base.a, 0.0, 1.0) : 1.0;
    o_color = vec4(col * alpha, alpha);
}
"#;

/// Hologram fill, plain lines, the atmosphere and the depth pre-pass (`u_mode` 0–3).
const FS_HOLO: &str = r#"
in vec3 v_world;
in vec3 v_nrm;
in vec3 v_onrm;
in vec2 v_uv;
in vec4 v_tan;
in vec3 v_obj;
out vec4 o_color;
uniform int u_mode;
uniform vec3 u_color;
uniform float u_strength;
uniform float u_time;
uniform vec3 u_eye;
uniform vec3 u_key;
void main() {
    if (u_mode == 3) {
        o_color = vec4(0.0);
        return;
    }
    if (u_mode == 1) {
        o_color = vec4(u_color * u_strength, u_strength);
        return;
    }
    vec3 n = normalize(v_nrm);
    vec3 v = normalize(u_eye - v_world);
    float nv = abs(dot(n, v));
    if (u_mode == 2) {
        float rim = pow(1.0 - nv, 2.6);
        float lit = clamp(dot(n, u_key) * 0.8 + 0.4, 0.0, 1.0);
        vec3 c = u_color * rim * lit * u_strength;
        o_color = vec4(c, max(c.r, max(c.g, c.b)));
        return;
    }
    float fres = pow(1.0 - nv, 2.0);
    float scan = 0.6 + 0.4 * sin(v_world.y * 150.0 - u_time * 3.0);
    float sweep = fract(v_world.y * 0.35 - u_time * 0.25);
    float band = smoothstep(0.0, 0.015, sweep) * (1.0 - smoothstep(0.015, 0.05, sweep));
    float i = (0.05 + fres * 0.85) * scan + band * 0.35;
    vec3 c = u_color * i * u_strength;
    o_color = vec4(c, clamp(i * 0.75, 0.0, 1.0));
}
"#;

const VS_GROUND: &str = r#"
in vec2 a_pos;
uniform mat4 u_viewproj;
uniform float u_y;
uniform float u_size;
out vec2 v_xz;
void main() {
    v_xz = a_pos * u_size;
    gl_Position = u_viewproj * vec4(v_xz.x, u_y, v_xz.y, 1.0);
}
"#;

const FS_GROUND: &str = r#"
in vec2 v_xz;
out vec4 o_color;
uniform vec3 u_accent;
uniform float u_shadow;
uniform float u_holo;
uniform float u_time;
uniform float u_size;
void main() {
    float r = length(v_xz);
    float fade = 1.0 - smoothstep(u_size * 0.4, u_size, r);
    vec2 gp = v_xz * 4.0;
    vec2 g = abs(fract(gp - 0.5) - 0.5) / max(fwidth(gp), vec2(1e-4));
    float line = 1.0 - min(min(g.x, g.y), 1.0);
    float fw = max(fwidth(r), 1e-4);
    float ring = 1.0 - smoothstep(0.0, 1.5 * fw, abs(r - u_shadow * 1.35));
    float ring2 = 1.0 - smoothstep(0.0, 1.5 * fw, abs(r - u_shadow * 1.62));
    vec3 col;
    float a;
    if (u_holo > 0.5) {
        float pulse = 0.55 + 0.45 * sin(u_time * 2.0 - r * 6.0);
        float disc = (1.0 - smoothstep(0.0, u_shadow * 1.35, r)) * 0.10;
        float i = (line * 0.16 + ring * 0.9 + ring2 * 0.45 * pulse + disc) * fade;
        col = u_accent * i;
        a = clamp(i, 0.0, 1.0);
    } else {
        float shadow = (1.0 - smoothstep(0.0, u_shadow * 1.3, r)) * 0.55 + (1.0 - smoothstep(0.0, u_shadow * 0.7, r)) * 0.25;
        float floor_a = 0.30 * fade;
        float glow = (line * 0.06 + ring * 0.45) * fade;
        a = clamp(floor_a + shadow * fade, 0.0, 0.9);
        col = vec3(0.04, 0.045, 0.055) * floor_a + u_accent * glow;
        a = max(a, glow);
    }
    o_color = vec4(col, a);
}
"#;

const VS_QUAD: &str = r#"
in vec2 a_pos;
out vec2 v_uv;
void main() {
    v_uv = a_pos * 0.5 + 0.5;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
"#;

const FS_COMPOSITE: &str = r#"
in vec2 v_uv;
out vec4 o_color;
uniform sampler2D t_img;
void main() {
    o_color = texture(t_img, v_uv);
}
"#;

const MESH_UNIFORMS: &[&str] = &["u_viewproj", "u_model", "u_normal", "u_tri"];
const PBR_UNIFORMS: &[&str] = &[
    "u_base", "u_emissive", "u_metallic", "u_roughness", "u_nscale", "u_cutoff", "u_tile", "u_flags", "t_base", "t_mr", "t_nrm", "t_emis", "t_occ", "u_envrot", "u_eye", "u_accent", "u_key", "u_fill", "u_rim", "u_env", "u_keyk",
];
const HOLO_UNIFORMS: &[&str] = &["u_mode", "u_color", "u_strength", "u_time", "u_eye", "u_key"];
const GROUND_UNIFORMS: &[&str] = &["u_viewproj", "u_y", "u_size", "u_accent", "u_shadow", "u_holo", "u_time"];

struct Program {
    id: glow::Program,
    uniforms: HashMap<&'static str, glow::UniformLocation>,
}

impl Program {
    unsafe fn new(gl: &glow::Context, header: &str, vs: &str, fs: &str, attribs: &[&str], uniforms: &[&[&'static str]]) -> Result<Program, String> {
        let program = gl.create_program()?;
        let mut shaders = Vec::new();
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let shader = gl.create_shader(kind)?;
            gl.shader_source(shader, &format!("{header}{src}"));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                let log = gl.get_shader_info_log(shader);
                gl.delete_shader(shader);
                gl.delete_program(program);
                return Err(format!("a shader did not compile: {}", log.trim()));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        for (i, name) in attribs.iter().enumerate() {
            gl.bind_attrib_location(program, i as u32, name);
        }
        gl.link_program(program);
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        if !gl.get_program_link_status(program) {
            let log = gl.get_program_info_log(program);
            gl.delete_program(program);
            return Err(format!("a shader did not link: {}", log.trim()));
        }
        let mut map = HashMap::new();
        for list in uniforms {
            for name in *list {
                if let Some(loc) = gl.get_uniform_location(program, name) {
                    map.insert(*name, loc);
                }
            }
        }
        Ok(Program { id: program, uniforms: map })
    }

    fn u(&self, name: &str) -> Option<&glow::UniformLocation> {
        self.uniforms.get(name)
    }
}

// ------------------------------------------------------------------------------ objects ----

struct GpuPart {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ibo: glow::Buffer,
    count: i32,
}

struct GpuTexture {
    tex: glow::Texture,
    /// Keeps the pixels' allocation alive, so its address (the key) is never reused meanwhile.
    _keep: Arc<Vec<u8>>,
}

struct Target {
    w: i32,
    h: i32,
    msaa_fbo: Option<glow::Framebuffer>,
    msaa_color: Option<glow::Renderbuffer>,
    depth: glow::Renderbuffer,
    resolve_fbo: glow::Framebuffer,
    resolve_tex: glow::Texture,
}

impl Target {
    unsafe fn destroy(self, gl: &glow::Context) {
        if let Some(f) = self.msaa_fbo {
            gl.delete_framebuffer(f);
        }
        if let Some(r) = self.msaa_color {
            gl.delete_renderbuffer(r);
        }
        gl.delete_renderbuffer(self.depth);
        gl.delete_framebuffer(self.resolve_fbo);
        gl.delete_texture(self.resolve_tex);
    }
}

struct Renderer {
    es: bool,
    max_samples: i32,
    anisotropy: bool,
    pbr: Program,
    holo: Program,
    ground: Program,
    composite: Program,
    quad_vao: glow::VertexArray,
    quad_vbo: glow::Buffer,
    model_id: u64,
    parts: Vec<GpuPart>,
    triangles: usize,
    textures: HashMap<usize, GpuTexture>,
    target: Option<Target>,
}

/// Textures uploaded per frame at most (a big model's maps arrive over a few frames).
const UPLOADS_PER_FRAME: usize = 3;

impl Renderer {
    unsafe fn new(gl: &glow::Context) -> Result<Renderer, String> {
        let version = egui_glow::ShaderVersion::get(gl);
        let es = match version {
            egui_glow::ShaderVersion::Gl140 => false,
            egui_glow::ShaderVersion::Es300 => true,
            other => return Err(format!("OpenGL 3 is needed ({other:?})")),
        };
        let header = format!("{}{}", version.version_declaration(), if es { "precision highp float;\nprecision highp int;\n" } else { "" });
        let mesh_attribs = ["a_pos", "a_nrm", "a_uv", "a_tan", "a_col"];
        let pbr = Program::new(gl, &header, VS_MESH, FS_PBR, &mesh_attribs, &[MESH_UNIFORMS, PBR_UNIFORMS])?;
        let holo = Program::new(gl, &header, VS_MESH, FS_HOLO, &mesh_attribs, &[MESH_UNIFORMS, HOLO_UNIFORMS]).inspect_err(|_| gl.delete_program(pbr.id))?;
        let ground = Program::new(gl, &header, VS_GROUND, FS_GROUND, &["a_pos"], &[GROUND_UNIFORMS]).inspect_err(|_| {
            gl.delete_program(pbr.id);
            gl.delete_program(holo.id);
        })?;
        let composite = Program::new(gl, &header, VS_QUAD, FS_COMPOSITE, &["a_pos"], &[&["t_img"]]).inspect_err(|_| {
            gl.delete_program(pbr.id);
            gl.delete_program(holo.id);
            gl.delete_program(ground.id);
        })?;
        let quad_vao = gl.create_vertex_array()?;
        let quad_vbo = gl.create_buffer()?;
        gl.bind_vertex_array(Some(quad_vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad_vbo));
        let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes_of(&quad), glow::STATIC_DRAW);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        let max_samples = gl.get_parameter_i32(glow::MAX_SAMPLES).clamp(0, 8);
        let anisotropy = gl.supported_extensions().iter().any(|e| e.contains("texture_filter_anisotropic"));
        info!("[3D] GPU renderer ready ({}; {} MSAA samples at most)", if es { "GLES 3" } else { "OpenGL 3+" }, max_samples);
        Ok(Renderer { es, max_samples, anisotropy, pbr, holo, ground, composite, quad_vao, quad_vbo, model_id: 0, parts: Vec::new(), triangles: 0, textures: HashMap::new(), target: None })
    }

    unsafe fn destroy(mut self, gl: &glow::Context) {
        self.drop_meshes(gl);
        for (_, t) in self.textures.drain() {
            gl.delete_texture(t.tex);
        }
        if let Some(t) = self.target.take() {
            t.destroy(gl);
        }
        for p in [self.pbr.id, self.holo.id, self.ground.id, self.composite.id] {
            gl.delete_program(p);
        }
        gl.delete_vertex_array(self.quad_vao);
        gl.delete_buffer(self.quad_vbo);
    }

    unsafe fn drop_meshes(&mut self, gl: &glow::Context) {
        for p in self.parts.drain(..) {
            gl.delete_vertex_array(p.vao);
            gl.delete_buffer(p.vbo);
            gl.delete_buffer(p.ibo);
        }
        self.model_id = 0;
        self.triangles = 0;
    }

    /// The offscreen framebuffer for a `w`×`h` view (multisampled where the GPU can).
    unsafe fn ensure_target(&mut self, gl: &glow::Context, w: i32, h: i32) -> Result<(), String> {
        if self.target.as_ref().is_some_and(|t| t.w == w && t.h == h) {
            return Ok(());
        }
        if let Some(t) = self.target.take() {
            t.destroy(gl);
        }
        let wanted = if (w as i64 * h as i64) > 4_000_000 { 2 } else { 4 };
        let mut samples = wanted.min(self.max_samples);
        loop {
            match self.make_target(gl, w, h, samples) {
                Ok(t) => {
                    self.target = Some(t);
                    return Ok(());
                }
                Err(e) if samples > 0 => {
                    warn!("[3D] No {samples}x MSAA ({e}); drawing without it.");
                    samples = 0;
                }
                Err(e) => return Err(e),
            }
        }
    }

    unsafe fn make_target(&self, gl: &glow::Context, w: i32, h: i32, samples: i32) -> Result<Target, String> {
        let resolve_tex = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(resolve_tex));
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, w, h, 0, glow::RGBA, glow::UNSIGNED_BYTE, None);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        gl.bind_texture(glow::TEXTURE_2D, None);
        let resolve_fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(resolve_fbo));
        gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(resolve_tex), 0);
        let depth = gl.create_renderbuffer()?;
        gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
        let (mut msaa_fbo, mut msaa_color) = (None, None);
        if samples > 0 {
            gl.renderbuffer_storage_multisample(glow::RENDERBUFFER, samples, glow::DEPTH_COMPONENT24, w, h);
            let color = gl.create_renderbuffer()?;
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(color));
            gl.renderbuffer_storage_multisample(glow::RENDERBUFFER, samples, glow::RGBA8, w, h);
            let fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::RENDERBUFFER, Some(color));
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, Some(depth));
            msaa_fbo = Some(fbo);
            msaa_color = Some(color);
        } else {
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, w, h);
            gl.framebuffer_renderbuffer(glow::FRAMEBUFFER, glow::DEPTH_ATTACHMENT, glow::RENDERBUFFER, Some(depth));
        }
        gl.bind_renderbuffer(glow::RENDERBUFFER, None);
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        let resolve_ok = {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(resolve_fbo));
            gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE
        };
        let target = Target { w, h, msaa_fbo, msaa_color, depth, resolve_fbo, resolve_tex };
        if status != glow::FRAMEBUFFER_COMPLETE || !resolve_ok {
            target.destroy(gl);
            return Err(format!("the framebuffer is incomplete (0x{status:X})"));
        }
        Ok(target)
    }

    /// The scene's meshes and textures on the GPU (a new model replaces the old one).
    unsafe fn sync(&mut self, gl: &glow::Context, scene: &Scene) -> Result<(), String> {
        if self.model_id != scene.model_id {
            self.drop_meshes(gl);
            for part in &scene.model.parts {
                self.parts.push(upload_part(gl, part)?);
                self.triangles += part.triangles();
            }
            self.model_id = scene.model_id;
        }
        // Textures by their pixels' address: an edit that keeps a texture keeps it uploaded.
        let wanted: HashSet<usize> = scene.textures.iter().map(|t| Arc::as_ptr(&t.rgba) as usize).collect();
        let stale: Vec<usize> = self.textures.keys().filter(|k| !wanted.contains(k)).copied().collect();
        for k in stale {
            if let Some(t) = self.textures.remove(&k) {
                gl.delete_texture(t.tex);
            }
        }
        let mut budget = UPLOADS_PER_FRAME;
        for t in &scene.textures {
            let key = Arc::as_ptr(&t.rgba) as usize;
            if budget == 0 || self.textures.contains_key(&key) {
                continue;
            }
            if t.width == 0 || t.height == 0 || t.rgba.len() < (t.width * t.height * 4) as usize {
                continue;
            }
            let tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, t.width as i32, t.height as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, Some(&t.rgba[..(t.width * t.height * 4) as usize]));
            gl.generate_mipmap(glow::TEXTURE_2D);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR_MIPMAP_LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::REPEAT as i32);
            if self.anisotropy {
                gl.tex_parameter_f32(glow::TEXTURE_2D, glow::TEXTURE_MAX_ANISOTROPY_EXT, 8.0);
            }
            gl.bind_texture(glow::TEXTURE_2D, None);
            self.textures.insert(key, GpuTexture { tex, _keep: Arc::clone(&t.rgba) });
            budget -= 1;
        }
        Ok(())
    }

    fn texture(&self, scene: &Scene, index: Option<usize>) -> Option<glow::Texture> {
        let t = scene.textures.get(index?)?;
        self.textures.get(&(Arc::as_ptr(&t.rgba) as usize)).map(|g| g.tex)
    }

    unsafe fn draw(&mut self, gl: &glow::Context, info: &egui::PaintCallbackInfo, f: &Frame) -> Result<(), String> {
        let vp = info.viewport_in_pixels();
        let clip = info.clip_rect_in_pixels();
        let (w, h) = (vp.width_px.clamp(1, 8192), vp.height_px.clamp(1, 8192));
        if clip.width_px <= 0 || clip.height_px <= 0 {
            return Ok(());
        }
        let previous = gl.get_parameter_framebuffer(glow::FRAMEBUFFER_BINDING);
        self.ensure_target(gl, w, h)?;
        self.sync(gl, &f.scene)?;
        let Some(target) = self.target.as_ref() else { return Ok(()) };
        let draw_fbo = target.msaa_fbo.unwrap_or(target.resolve_fbo);
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(draw_fbo));
        gl.viewport(0, 0, w, h);
        gl.disable(glow::SCISSOR_TEST);
        gl.color_mask(true, true, true, true);
        gl.depth_mask(true);
        gl.clear_color(0.0, 0.0, 0.0, 0.0);
        gl.clear_depth_f32(1.0);
        gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
        gl.enable(glow::DEPTH_TEST);
        gl.depth_func(glow::LEQUAL);
        gl.disable(glow::CULL_FACE);
        gl.disable(glow::BLEND);
        let viewproj = math::mul(&f.proj, &f.view);
        match f.look {
            Look::Realistic => {
                self.pass_pbr(gl, f, &viewproj, false);
                if !f.scene.planet {
                    self.pass_ground(gl, f, &viewproj, false);
                }
                self.pass_pbr(gl, f, &viewproj, true);
                if let Some(atmo) = f.scene.atmosphere {
                    self.pass_atmosphere(gl, f, &viewproj, atmo);
                }
                if f.wireframe {
                    self.pass_lines(gl, f, &viewproj, 0.55, false);
                }
            }
            Look::Hologram => {
                self.pass_holo(gl, f, &viewproj, 3);
                if !f.scene.planet {
                    self.pass_ground(gl, f, &viewproj, true);
                }
                self.pass_holo(gl, f, &viewproj, 0);
                if f.wireframe || self.triangles < 300_000 {
                    self.pass_lines(gl, f, &viewproj, if f.wireframe { 0.5 } else { 0.16 }, true);
                }
            }
        }
        if let Some(msaa) = target.msaa_fbo {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(msaa));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(target.resolve_fbo));
            gl.blit_framebuffer(0, 0, w, h, 0, 0, w, h, glow::COLOR_BUFFER_BIT, glow::NEAREST);
        }
        // Over the block.
        gl.bind_framebuffer(glow::FRAMEBUFFER, previous);
        gl.viewport(vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px);
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(clip.left_px, clip.from_bottom_px, clip.width_px, clip.height_px);
        gl.disable(glow::DEPTH_TEST);
        gl.depth_mask(false);
        gl.enable(glow::BLEND);
        gl.blend_equation(glow::FUNC_ADD);
        gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE_MINUS_DST_ALPHA, glow::ONE);
        gl.use_program(Some(self.composite.id));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(target.resolve_tex));
        gl.uniform_1_i32(self.composite.u("t_img"), 0);
        gl.bind_vertex_array(Some(self.quad_vao));
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.bind_vertex_array(None);
        gl.bind_texture(glow::TEXTURE_2D, None);
        Ok(())
    }

    /// The matrix of part `i` (pulled away from the centre when exploded).
    fn part_matrix(f: &Frame, i: usize) -> math::M4 {
        if f.explode <= 0.001 {
            return f.model;
        }
        let c = f.scene.bounds.centre();
        let pc = f.scene.part_centres.get(i).copied().unwrap_or(c);
        let off = math::scale(math::sub(pc, c), f.explode * 0.9);
        math::mul(&f.model, &math::translate(off))
    }

    unsafe fn set_mesh_uniforms(gl: &glow::Context, p: &Program, viewproj: &math::M4, model: &math::M4, tri: &math::M4) {
        gl.uniform_matrix_4_f32_slice(p.u("u_viewproj"), false, viewproj);
        gl.uniform_matrix_4_f32_slice(p.u("u_model"), false, model);
        gl.uniform_matrix_4_f32_slice(p.u("u_normal"), false, &math::normal_matrix(model));
        gl.uniform_matrix_4_f32_slice(p.u("u_tri"), false, tri);
    }

    /// Camera-relative studio lights (world space).
    fn lights(f: &Frame) -> ([f32; 3], [f32; 3], [f32; 3]) {
        let turn = math::rot_y(f.cam_yaw);
        let d = |v: [f32; 3]| math::normalize(math::dir(&turn, v));
        if f.scene.planet {
            (d([-1.0, 0.18, 0.55]), d([0.9, 0.2, 0.3]), d([0.3, 0.2, -1.0]))
        } else {
            (d([-0.55, 0.75, 0.75]), d([0.95, 0.3, 0.2]), d([-0.15, 0.4, -1.0]))
        }
    }

    unsafe fn pass_pbr(&self, gl: &glow::Context, f: &Frame, viewproj: &math::M4, blended: bool) {
        let p = &self.pbr;
        let scene = &f.scene;
        gl.use_program(Some(p.id));
        let (key, fill, rim) = Self::lights(f);
        gl.uniform_matrix_4_f32_slice(p.u("u_envrot"), false, &math::rot_y(-f.cam_yaw));
        gl.uniform_3_f32(p.u("u_eye"), f.eye[0], f.eye[1], f.eye[2]);
        gl.uniform_3_f32(p.u("u_accent"), f.accent[0], f.accent[1], f.accent[2]);
        gl.uniform_3_f32(p.u("u_key"), key[0], key[1], key[2]);
        gl.uniform_3_f32(p.u("u_fill"), fill[0], fill[1], fill[2]);
        gl.uniform_3_f32(p.u("u_rim"), rim[0], rim[1], rim[2]);
        gl.uniform_1_f32(p.u("u_env"), if scene.planet { 0.07 } else { 1.0 });
        gl.uniform_1_f32(p.u("u_keyk"), if scene.planet { 1.6 } else { 1.0 });
        for (unit, name) in ["t_base", "t_mr", "t_nrm", "t_emis", "t_occ"].iter().enumerate() {
            gl.uniform_1_i32(p.u(name), unit as i32 + 1);
        }
        let tri = scene.normalise();
        if blended {
            gl.enable(glow::BLEND);
            gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            gl.depth_mask(false);
        } else {
            gl.disable(glow::BLEND);
            gl.depth_mask(true);
        }
        for (i, gp) in self.parts.iter().enumerate() {
            let Some(mat) = scene.part_material.get(i).and_then(|m| scene.materials.get(*m)) else { continue };
            if (mat.alpha == AlphaMode::Blend) != blended {
                continue;
            }
            let model = Self::part_matrix(f, i);
            Self::set_mesh_uniforms(gl, p, viewproj, &model, &tri);
            self.bind_material(gl, p, scene, mat);
            gl.bind_vertex_array(Some(gp.vao));
            gl.draw_elements(glow::TRIANGLES, gp.count, glow::UNSIGNED_INT, 0);
        }
        gl.bind_vertex_array(None);
        gl.depth_mask(true);
        gl.disable(glow::BLEND);
    }

    unsafe fn bind_material(&self, gl: &glow::Context, p: &Program, scene: &Scene, m: &Material) {
        let mut flags = 0i32;
        let slots = [(m.base_color_tex, 1), (m.metallic_roughness_tex, 2), (m.normal_tex, 4), (m.emissive_tex, 8), (m.occlusion_tex, 16)];
        for (unit, (index, bit)) in slots.iter().enumerate() {
            gl.active_texture(glow::TEXTURE1 + unit as u32);
            let tex = self.texture(scene, *index);
            gl.bind_texture(glow::TEXTURE_2D, tex);
            if tex.is_some() {
                flags |= bit;
            }
        }
        gl.active_texture(glow::TEXTURE0);
        if m.triplanar {
            flags |= 32;
        }
        if m.vertex_colors {
            flags |= 512;
        }
        let cutoff = match m.alpha {
            AlphaMode::Mask(c) => {
                flags |= 64;
                c
            }
            AlphaMode::Blend => {
                flags |= 256;
                0.0
            }
            AlphaMode::Opaque => 0.0,
        };
        if scene.planet && m.emissive_tex.is_some() && m.name != "sun" {
            flags |= 128;
        }
        let b = m.base_color;
        gl.uniform_4_f32(p.u("u_base"), b[0], b[1], b[2], b[3]);
        gl.uniform_3_f32(p.u("u_emissive"), m.emissive[0], m.emissive[1], m.emissive[2]);
        gl.uniform_1_f32(p.u("u_metallic"), m.metallic);
        gl.uniform_1_f32(p.u("u_roughness"), m.roughness);
        gl.uniform_1_f32(p.u("u_nscale"), m.normal_scale);
        gl.uniform_1_f32(p.u("u_cutoff"), cutoff);
        gl.uniform_1_f32(p.u("u_tile"), if m.tile > 0.0 { m.tile } else { 1.0 });
        gl.uniform_1_i32(p.u("u_flags"), flags);
    }

    /// The hologram's passes: 3 the depth pre-pass, 0 the additive fill.
    unsafe fn pass_holo(&self, gl: &glow::Context, f: &Frame, viewproj: &math::M4, mode: i32) {
        let p = &self.holo;
        gl.use_program(Some(p.id));
        let (key, _, _) = Self::lights(f);
        gl.uniform_1_i32(p.u("u_mode"), mode);
        gl.uniform_3_f32(p.u("u_color"), f.accent[0], f.accent[1], f.accent[2]);
        gl.uniform_1_f32(p.u("u_strength"), 1.0);
        gl.uniform_1_f32(p.u("u_time"), f.time);
        gl.uniform_3_f32(p.u("u_eye"), f.eye[0], f.eye[1], f.eye[2]);
        gl.uniform_3_f32(p.u("u_key"), key[0], key[1], key[2]);
        if mode == 3 {
            gl.color_mask(false, false, false, false);
            gl.depth_mask(true);
            gl.disable(glow::BLEND);
        } else {
            gl.color_mask(true, true, true, true);
            gl.depth_mask(false);
            gl.enable(glow::BLEND);
            gl.blend_func(glow::ONE, glow::ONE);
        }
        let tri = f.scene.normalise();
        for (i, gp) in self.parts.iter().enumerate() {
            let model = Self::part_matrix(f, i);
            Self::set_mesh_uniforms(gl, p, viewproj, &model, &tri);
            gl.bind_vertex_array(Some(gp.vao));
            gl.draw_elements(glow::TRIANGLES, gp.count, glow::UNSIGNED_INT, 0);
        }
        gl.bind_vertex_array(None);
        gl.color_mask(true, true, true, true);
        gl.depth_mask(true);
        gl.disable(glow::BLEND);
    }

    /// The mesh's edges over what is drawn (desktop OpenGL only: GLES has no line fill mode).
    unsafe fn pass_lines(&self, gl: &glow::Context, f: &Frame, viewproj: &math::M4, strength: f32, additive: bool) {
        if self.es {
            return;
        }
        let p = &self.holo;
        gl.use_program(Some(p.id));
        gl.uniform_1_i32(p.u("u_mode"), 1);
        let c = if additive { f.accent } else { [f.accent[0] * 0.6 + 0.4, f.accent[1] * 0.6 + 0.4, f.accent[2] * 0.6 + 0.4] };
        gl.uniform_3_f32(p.u("u_color"), c[0], c[1], c[2]);
        gl.uniform_1_f32(p.u("u_strength"), strength);
        gl.polygon_mode(glow::FRONT_AND_BACK, glow::LINE);
        gl.enable(glow::POLYGON_OFFSET_LINE);
        gl.polygon_offset(-1.0, -1.0);
        gl.depth_mask(false);
        gl.enable(glow::BLEND);
        if additive {
            gl.blend_func(glow::ONE, glow::ONE);
        } else {
            gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
        }
        let tri = f.scene.normalise();
        for (i, gp) in self.parts.iter().enumerate() {
            let model = Self::part_matrix(f, i);
            Self::set_mesh_uniforms(gl, p, viewproj, &model, &tri);
            gl.bind_vertex_array(Some(gp.vao));
            gl.draw_elements(glow::TRIANGLES, gp.count, glow::UNSIGNED_INT, 0);
        }
        gl.bind_vertex_array(None);
        gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
        gl.disable(glow::POLYGON_OFFSET_LINE);
        gl.depth_mask(true);
        gl.disable(glow::BLEND);
    }

    /// A planet's atmosphere: a slightly larger shell, lit at the limb.
    unsafe fn pass_atmosphere(&self, gl: &glow::Context, f: &Frame, viewproj: &math::M4, colour: [f32; 3]) {
        let Some(gp) = self.parts.first() else { return };
        let p = &self.holo;
        gl.use_program(Some(p.id));
        let (key, _, _) = Self::lights(f);
        gl.uniform_1_i32(p.u("u_mode"), 2);
        gl.uniform_3_f32(p.u("u_color"), colour[0], colour[1], colour[2]);
        gl.uniform_1_f32(p.u("u_strength"), 1.1);
        gl.uniform_3_f32(p.u("u_eye"), f.eye[0], f.eye[1], f.eye[2]);
        gl.uniform_3_f32(p.u("u_key"), key[0], key[1], key[2]);
        let c = f.scene.model.parts[0].bounds().centre();
        let shell = math::mul(&f.model, &math::mul(&math::translate(c), &math::mul(&math::scaling([1.035; 3]), &math::translate(math::scale(c, -1.0)))));
        Self::set_mesh_uniforms(gl, p, viewproj, &shell, &math::identity());
        gl.depth_mask(false);
        gl.enable(glow::BLEND);
        gl.blend_func(glow::ONE, glow::ONE);
        gl.bind_vertex_array(Some(gp.vao));
        gl.draw_elements(glow::TRIANGLES, gp.count, glow::UNSIGNED_INT, 0);
        gl.bind_vertex_array(None);
        gl.depth_mask(true);
        gl.disable(glow::BLEND);
    }

    unsafe fn pass_ground(&self, gl: &glow::Context, f: &Frame, viewproj: &math::M4, holo: bool) {
        let p = &self.ground;
        gl.use_program(Some(p.id));
        gl.uniform_matrix_4_f32_slice(p.u("u_viewproj"), false, viewproj);
        gl.uniform_1_f32(p.u("u_y"), f.ground_y - 0.002);
        gl.uniform_1_f32(p.u("u_size"), 2.6);
        gl.uniform_3_f32(p.u("u_accent"), f.accent[0], f.accent[1], f.accent[2]);
        gl.uniform_1_f32(p.u("u_shadow"), f.footprint.clamp(0.08, 1.4));
        gl.uniform_1_f32(p.u("u_holo"), if holo { 1.0 } else { 0.0 });
        gl.uniform_1_f32(p.u("u_time"), f.time);
        gl.depth_mask(false);
        gl.enable(glow::BLEND);
        gl.blend_func_separate(glow::ONE, glow::ONE_MINUS_SRC_ALPHA, glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
        gl.bind_vertex_array(Some(self.quad_vao));
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        gl.bind_vertex_array(None);
        gl.depth_mask(true);
        gl.disable(glow::BLEND);
    }
}

fn bytes_of<T: Copy>(v: &[T]) -> &[u8] {
    // (Plain numbers only: f32 / u32.)
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// One part as an interleaved vertex buffer: position, normal, uv, tangent, colour (15 floats).
unsafe fn upload_part(gl: &glow::Context, part: &crate::mesh_io::Part) -> Result<GpuPart, String> {
    let n = part.positions.len();
    let mut data: Vec<f32> = Vec::with_capacity(n * 15);
    for i in 0..n {
        let p = part.positions[i];
        let nr = part.normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]);
        let uv = part.uvs.get(i).copied().unwrap_or([0.0, 0.0]);
        let t = part.tangents.get(i).copied().unwrap_or([0.0; 4]);
        let c = part.colors.get(i).copied().unwrap_or([1.0; 3]);
        data.extend_from_slice(&[p[0], p[1], p[2], nr[0], nr[1], nr[2], uv[0], uv[1], t[0], t[1], t[2], t[3], c[0], c[1], c[2]]);
    }
    // Only indices that point at a vertex.
    let indices: Vec<u32> = if part.indices.iter().all(|&i| (i as usize) < n) {
        part.indices.clone()
    } else {
        part.indices.chunks_exact(3).filter(|t| t.iter().all(|&i| (i as usize) < n)).flatten().copied().collect()
    };
    let vao = gl.create_vertex_array()?;
    let vbo = gl.create_buffer()?;
    let ibo = gl.create_buffer()?;
    gl.bind_vertex_array(Some(vao));
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes_of(&data), glow::STATIC_DRAW);
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ibo));
    gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, bytes_of(&indices), glow::STATIC_DRAW);
    let stride = 15 * 4;
    for (loc, size, offset) in [(0u32, 3i32, 0i32), (1, 3, 12), (2, 2, 24), (3, 4, 32), (4, 3, 48)] {
        gl.enable_vertex_attrib_array(loc);
        gl.vertex_attrib_pointer_f32(loc, size, glow::FLOAT, false, stride, offset);
    }
    gl.bind_vertex_array(None);
    gl.bind_buffer(glow::ARRAY_BUFFER, None);
    // (The element buffer stays the VAO's.)
    gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
    Ok(GpuPart { vao, vbo, ibo, count: indices.len() as i32 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaders_declare_what_the_renderer_sets() {
        // Every uniform the renderer looks up is declared in its shaders (a typo would draw
        // nothing without an error on most drivers).
        for name in PBR_UNIFORMS {
            assert!(FS_PBR.contains(&format!(" {name};")), "{name}");
        }
        for name in MESH_UNIFORMS {
            assert!(VS_MESH.contains(&format!(" {name};")), "{name}");
        }
        for name in HOLO_UNIFORMS {
            assert!(FS_HOLO.contains(&format!(" {name};")), "{name}");
        }
        for name in GROUND_UNIFORMS {
            assert!(VS_GROUND.contains(&format!(" {name};")) || FS_GROUND.contains(&format!(" {name};")), "{name}");
        }
        // The vertex shader's outputs are the fragment shaders' inputs.
        for v in ["v_world", "v_nrm", "v_onrm", "v_uv", "v_tan", "v_obj"] {
            assert!(FS_PBR.contains(&format!("in vec")) && (FS_PBR.contains(&format!(" {v};"))), "{v}");
            assert!(FS_HOLO.contains(&format!(" {v};")), "{v}");
        }
        // (Vertex colours only tint the product-shot look.)
        assert!(VS_MESH.contains("out vec3 v_col;") && FS_PBR.contains("in vec3 v_col;"));
        assert_eq!(status(), Status::Unknown);
    }
}
