#version 300 es
precision highp float;
// Adds a gaussian blob of `u_color` (a force for the velocity field, an ink colour for the dye).
uniform sampler2D u_base;
uniform vec2 u_res;
uniform vec2 u_point;   // 0..1, y up
uniform vec3 u_color;
uniform float u_radius;
uniform float u_aspect;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy / u_res;
    vec2 p = uv - u_point;
    p.x *= u_aspect;
    o = vec4(texture(u_base, uv).rgb + exp(-dot(p, p) / u_radius) * u_color, 1.0);
}
