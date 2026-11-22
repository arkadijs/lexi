package app.lexi

import android.content.Context

/**
 * Who answers: OpenAI Realtime (WebRTC) or Google Gemini Live (WebSocket). Chosen per call, so the selector is
 * locked during a call. The voice list depends on the provider ([Voices]); the model choice ([Models]) only
 * applies to OpenAI (Gemini has one Live model, fixed by the server).
 */
object Providers {
    class Provider(val id: String, val label: String)

    val all = listOf(
        Provider("openai", "OpenAI"),
        Provider("google", "Google"),
    )

    fun current(context: Context): Provider {
        val id = context.getSharedPreferences("lexi", Context.MODE_PRIVATE).getString("provider", all.first().id)
        return all.firstOrNull { it.id == id } ?: all.first()
    }

    fun set(context: Context, provider: Provider) {
        context.getSharedPreferences("lexi", Context.MODE_PRIVATE).edit().putString("provider", provider.id).apply()
    }
}
