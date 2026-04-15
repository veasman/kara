#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
uniform vec2 direction;
uniform float spread;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// Separable 9-tap Gaussian, sigma ~1.5, weights sum to 1.0.
// `direction` is a texel-space step vector: (1.0/w, 0.0) for the horizontal
// pass, (0.0, 1.0/h) for the vertical pass. `spread` multiplies the step
// to cheaply widen the effective kernel — 4.0 gives a "heavy" blur at
// 9-tap cost, which is what the scratchpad backdrop needs.
void main() {
    vec2 step = direction * spread;
    vec4 color = vec4(0.0);
    color += texture2D(tex, v_coords + step * -4.0) * 0.0162;
    color += texture2D(tex, v_coords + step * -3.0) * 0.0540;
    color += texture2D(tex, v_coords + step * -2.0) * 0.1216;
    color += texture2D(tex, v_coords + step * -1.0) * 0.1946;
    color += texture2D(tex, v_coords              ) * 0.2270;
    color += texture2D(tex, v_coords + step *  1.0) * 0.1946;
    color += texture2D(tex, v_coords + step *  2.0) * 0.1216;
    color += texture2D(tex, v_coords + step *  3.0) * 0.0540;
    color += texture2D(tex, v_coords + step *  4.0) * 0.0162;

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
