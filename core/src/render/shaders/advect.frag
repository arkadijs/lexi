#version 300 es
precision highp float;
// Semi-Lagrangian advection: look back along the velocity field and sample there.
uniform sampler2D u_vel;     // velocity, in grid cells per second
uniform sampler2D u_src;     // the field being advected (velocity or dye)
uniform vec2 u_dst_res;      // size of the target in pixels
uniform vec2 u_vel_texel;    // 1 / velocity grid size
uniform float u_dt;
uniform float u_dissipation;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy / u_dst_res;
    vec2 coord = uv - u_dt * texture(u_vel, uv).xy * u_vel_texel;
    o = texture(u_src, coord) / (1.0 + u_dissipation * u_dt);
}
