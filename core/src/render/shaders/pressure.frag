#version 300 es
precision highp float;
// One Jacobi iteration of the pressure Poisson equation.
uniform sampler2D u_pressure;
uniform sampler2D u_div;
uniform vec2 u_texel;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy * u_texel;
    float L = texture(u_pressure, uv - vec2(u_texel.x, 0.0)).x;
    float R = texture(u_pressure, uv + vec2(u_texel.x, 0.0)).x;
    float T = texture(u_pressure, uv + vec2(0.0, u_texel.y)).x;
    float B = texture(u_pressure, uv - vec2(0.0, u_texel.y)).x;
    o = vec4((L + R + B + T - texture(u_div, uv).x) * 0.25, 0.0, 0.0, 1.0);
}
