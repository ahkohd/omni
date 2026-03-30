struct VertexInput {
  a0: vec2<f32>,
  a1: vec2<f32>,
};

struct VertexOutput {
  @builtin(position) position: vec4<f32>,
  @location(0) uv: vec2<f32>,
};

struct Uniforms {
  time: f32,
  energy: f32,
  opacity: f32,
  distortion: f32,
  speed: f32,
  activity: f32,
  fill_ratio: f32, // formerly pad0: 1.0 = empty state, 0.0 = full text
  pad1: f32,
};

var<uniform> b0: Uniforms;

fn hash(p: vec2<f32>) -> f32 {
    let p2 = fract(p * vec2<f32>(5.3983, 5.4427));
    let d = dot(p2.yx, p2.xy + vec2<f32>(21.5351, 14.3137));
    let p3 = p2 + vec2<f32>(d, d);
    return fract(p3.x * p3.y * 95.4337);
}

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
  var out: VertexOutput;
  out.position = vec4<f32>(input.a0, 0.0, 1.0);
  out.uv = input.a1;
  return out;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
  let uv = input.uv;

  let x = uv.x;
  let y = 1.0 - uv.y + 0.02;
  // Increase energy scaling base so bounds react more aggressively
  let e = sqrt(clamp(b0.energy, 0.0, 1.0)) * 0.70;
  let t = b0.time;

  // -- Blobby uneven top edge — like audio amplitude peaks --
  // Multiple overlapping bumps at different scales, all moving.
  let base_h_default = 0.14 + e * 0.35;
  let base_h_full = 0.85 + e * 0.15; 
  let base_h = mix(base_h_default, base_h_full, b0.fill_ratio);
  
  // We no longer multiply t by spd directly because t (b0.time) is already exponentially integrated with audio speed in Rust!
  // Removing it avoids massive derivative phase jumps (stalling) during volume spikes.
  
  // Audio-driven X-warp for erratic, aquatic horizontal swimming!
  let x_shift = e * 0.08;

  let raw_bumps = 0.0
    + sin((x + sin(t * 3.1) * x_shift) * 5.0  + t * 0.9) * 0.30
    + sin((x - cos(t * 4.7) * x_shift) * 9.0  - t * 0.6) * 0.22
    + sin((x + sin(t * 7.4) * x_shift) * 16.0 + t * 1.1) * 0.12
    + sin((x - cos(t * 2.3) * x_shift) * 3.5  - t * 0.4) * 0.20
    + sin((x + sin(t * 5.5) * x_shift) * 7.0  + t * 0.7) * 0.16;
  
  // Normalize raw bumps cleanly between 0 and 1
  let bump_norm = clamp(raw_bumps * 0.5 + 0.5, 0.0, 1.0);

  // Center convergence: Reduce horizontal squishing
  let cx = (x - 0.5) * 2.0; 
  let center_weight = 1.0 - cx * cx; 
  let convergence = mix(1.0, center_weight, e * 0.25); 

  // Make bumps physically splash higher based on audio energy
  let dynamic_bump_amp = 1.0 + e * 1.5;

  // Stable resting baseline, but deeper valleys when loud to emphasize sharp peaks
  let valley = 0.45 - e * 0.35;
  // Apply dynamic bump amp mathematically here so it never flatline-clips from the raw bump clamp!
  let dynamic_height = bump_norm * (1.0 - valley) * dynamic_bump_amp;
  let local_h = base_h * (valley + dynamic_height) * convergence;

  // Soft glow falloff from bottom.
  let v = y / max(local_h, 0.001);
  let front_glow = clamp(1.0 - v, 0.0, 1.0) * clamp(1.0 - v, 0.0, 1.0);

  // -- GHOST LAYER CALCULATION --
  // A secondary, dimmer wave sitting behind the main one.
  let back_bumps = 0.0
    + sin((x - sin(t * 4.2) * x_shift) * 8.0 - t * 0.7) * 0.25
    + sin((x + cos(t * 6.8) * x_shift) * 14.0 + t * 0.8) * 0.15;
  let back_bump_norm = clamp(back_bumps * 0.5 + 0.5, 0.0, 1.0);
  let back_dynamic = back_bump_norm * (1.0 - valley) * dynamic_bump_amp;
  let back_local_h = base_h * 1.15 * (valley + back_dynamic) * convergence;
  
  let back_v = y / max(back_local_h, 0.001);
  let back_glow = clamp(1.0 - back_v, 0.0, 1.0) * clamp(1.0 - back_v, 0.0, 1.0) * 0.45; // dimmer

  let glow = max(front_glow, back_glow);

  // -- VIGNETTE --
  // Dim the left and right extreme edges so it pools in the middle
  let edge_fade = smoothstep(0.0, 0.15, x) * smoothstep(1.0, 0.85, x);
  let vignette = mix(0.40, 1.0, edge_fade);

  // -- Colors: continuously flowing, not static --
  // The key: color positions themselves move over time at a visible pace.
  let drift = t * 0.35;

  // Warp the x coordinate so color zones actively slide and merge.
  let warped_x = x
    + sin(t * 0.23 + x * 1.5) * 0.18
    + sin(t * 0.17 - x * 2.8) * 0.12
    + sin(t * 0.31 + x * 0.7) * 0.08;

  let phase = fract(warped_x * 0.5 + drift * 0.1);

  // Saturation and Brightness bloom when loud
  let sat = 0.65 + e * 0.6;
  let brightness = 1.0 + e * 0.35;

  // Modern Cyber-Palette with Hue Shifting during high energy
  let col_blue    = vec3<f32>(0.12, 0.22, 0.95) * sat * brightness; 
  let col_magenta = mix(vec3<f32>(0.85, 0.12, 0.58), vec3<f32>(1.0, 0.20, 0.85), e) * sat * brightness; 
  let col_cyan    = mix(vec3<f32>(0.12, 0.85, 0.92), vec3<f32>(0.60, 1.0, 1.0), e) * sat * brightness; 
  let col_purple  = vec3<f32>(0.42, 0.08, 0.82) * sat * brightness;

  let p4 = phase * 4.0;
  let seg = u32(p4) % 4u;
  let f = fract(p4);
  let sf = f * f * (3.0 - 2.0 * f);

  var color: vec3<f32>;
  if (seg == 0u) {
    color = mix(col_blue, col_magenta, sf);
  } else if (seg == 1u) {
    color = mix(col_magenta, col_cyan, sf);
  } else if (seg == 2u) {
    color = mix(col_cyan, col_purple, sf);
  } else {
    color = mix(col_purple, col_blue, sf);
  }

  // -- FILM GRAIN OVERLAY --
  // Adds microscopic noise to break up banding and make it look like frosted glass
  let noise = (hash(uv * vec2<f32>(100.0, 100.0) + vec2<f32>(t, -t)) - 0.5) * 0.06;
  color = color + vec3<f32>(noise, noise, noise);

  let bottom_fade = smoothstep(0.0, 0.02, y);
  let alpha = glow * bottom_fade * vignette * b0.opacity;

  return vec4<f32>(color, clamp(alpha, 0.0, 1.0));
}
