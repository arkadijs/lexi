#version 300 es
precision highp float;
uniform sampler2D u_vel;
uniform vec2 u_texel;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy * u_texel;
    float L = texture(u_vel, uv - vec2(u_texel.x, 0.0)).y;
    float R = texture(u_vel, uv + vec2(u_texel.x, 0.0)).y;
    float T = texture(u_vel, uv + vec2(0.0, u_texel.y)).x;
    float B = texture(u_vel, uv - vec2(0.0, u_texel.y)).x;
    o = vec4(0.5 * (R - L - T + B), 0.0, 0.0, 1.0);
}
