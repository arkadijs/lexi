package app.lexi

import android.content.Context

/**
 * The Realtime models offered in the selector: the cheap default and the full one. The service must list both
 * in LEXI_MODELS; prices live in the core (pricing module). Chosen per call, so the selector is locked
 * while a call runs.
 */
object Models {
    class Model(val id: String, val label: String)

    val all = listOf(
        Model("gpt-realtime-2.1-mini", "Mini"),
        Model("gpt-realtime-2.1", "Smart"),
    )

    fun current(context: Context): Model {
        val id = context.getSharedPreferences("lexi", Context.MODE_PRIVATE).getString("model", all.first().id)
        return all.firstOrNull { it.id == id } ?: all.first()
    }

    fun set(context: Context, model: Model) {
        context.getSharedPreferences("lexi", Context.MODE_PRIVATE).edit().putString("model", model.id).apply()
    }
}
