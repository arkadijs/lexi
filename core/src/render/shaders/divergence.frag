#version 300 es
precision highp float;
uniform sampler2D u_vel;
uniform vec2 u_texel;
out vec4 o;
void main() {
    vec2 uv = gl_FragCoord.xy * u_texel;
    vec2 L = texture(u_vel, uv - vec2(u_texel.x, 0.0)).xy;
    vec2 R = texture(u_vel, uv + vec2(u_texel.x, 0.0)).xy;
    vec2 T = texture(u_vel, uv + vec2(0.0, u_texel.y)).xy;
    vec2 B = texture(u_vel, uv - vec2(0.0, u_texel.y)).xy;
    vec2 C = texture(u_vel, uv).xy;
    // Solid walls: the velocity reflects at the edges.
    if (uv.x - u_texel.x < 0.0) L.x = -C.x;
    if (uv.x + u_texel.x > 1.0) R.x = -C.x;
    if (uv.y + u_texel.y > 1.0) T.y = -C.y;
    if (uv.y - u_texel.y < 0.0) B.y = -C.y;
    o = vec4(0.5 * (R.x - L.x + T.y - B.y), 0.0, 0.0, 1.0);
}
