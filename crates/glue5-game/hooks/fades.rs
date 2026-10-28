// Hooks for the runtime's `fades.rs` (see ../hooks.rs).
&[
    // Every draw of a shader begins here, with its technique for the
    // technique group forced at the time (the fade pass forces one): no
    // technique, nothing drawn.
    ("shader_begin_draw", "rage::grmShader::BeginDraw(rage::grmShader::eDrawType, bool, rage::grcEffectTechnique__) const"),
    ("shader_end_draw", "rage::grmShader::EndDraw() const"),
    // A fading entity's alpha, as the fade pass sets it before its draws
    // (stipple fades), and back to none after.
    ("set_stipple_alpha", "CShaderLib::SetStippleAlpha(unsigned int, bool)"),
    ("reset_stipple_alpha", "CShaderLib::ResetStippleAlpha()"),
]
