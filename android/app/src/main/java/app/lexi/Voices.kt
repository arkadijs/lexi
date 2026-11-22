package app.lexi

import android.content.Context

/**
 * The voices offered in the selector, per provider (the server accepts more: all ten OpenAI voices and the
 * voices listed in LEXI_GEMINI_VOICES). The first entry of each list is its default; a stored choice that is no
 * longer offered falls back to it. Genders matter for languages that mark them (the server tells the bot):
 * Cedar and Charon/Fenrir are male, Marin, Alloy and Kore/Leda female.
 */
object Voices {
    class Voice(val id: String, val label: String)

    private val openai = listOf(
        Voice("cedar", "Cedar"),
        Voice("marin", "Marin"),
        Voice("alloy", "Alloy"),
    )
    private val google = listOf(
        Voice("charon", "Charon"),
        Voice("fenrir", "Fenrir"),
        Voice("kore", "Kore"),
        Voice("leda", "Leda"),
    )

    fun all(provider: Providers.Provider): List<Voice> = if (provider.id == "google") google else openai

    // OpenAI keeps the original preference key so an existing choice survives the update.
    private fun key(provider: Providers.Provider) = if (provider.id == "openai") "voice" else "voice_${provider.id}"

    fun current(context: Context): Voice {
        val provider = Providers.current(context)
        val list = all(provider)
        val id = context.getSharedPreferences("lexi", Context.MODE_PRIVATE).getString(key(provider), list.first().id)
        return list.firstOrNull { it.id == id } ?: list.first()
    }

    fun set(context: Context, voice: Voice) {
        val provider = Providers.current(context)
        context.getSharedPreferences("lexi", Context.MODE_PRIVATE).edit().putString(key(provider), voice.id).apply()
    }
}
