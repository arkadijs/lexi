#version 300 es
precision highp float;
// out = in * u_value (used to decay the previous pressure, which speeds up the next solve).
uniform sampler2D u_tex;
uniform vec2 u_res;
uniform float u_value;
out vec4 o;
void main() { o = texture(u_tex, gl_FragCoord.xy / u_res) * u_value; }
