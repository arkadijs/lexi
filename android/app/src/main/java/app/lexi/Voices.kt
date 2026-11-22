package app.lexi

import android.content.Context

/**
 * The voices offered in the selector (the minter accepts all ten built-in Realtime voices; the app only
 * offers these three). Cedar is the default. A stored choice that is no longer offered falls back to the first.
 */
object Voices {
    class Voice(val id: String, val label: String)

    val all = listOf(
        Voice("cedar", "Cedar"),
        Voice("marin", "Marin"),
        Voice("alloy", "Alloy"),
    )

    fun current(context: Context): Voice {
        val id = context.getSharedPreferences("lexi", Context.MODE_PRIVATE).getString("voice", all.first().id)
        return all.firstOrNull { it.id == id } ?: all.first()
    }

    fun set(context: Context, voice: Voice) {
        context.getSharedPreferences("lexi", Context.MODE_PRIVATE).edit().putString("voice", voice.id).apply()
    }
}
