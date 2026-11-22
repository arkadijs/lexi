package app.lexi

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.view.MotionEvent
import android.view.View

/** A round, custom-drawn button (no drawables, no libraries). Content descriptions are set by the owner. */
class IconButton(context: Context) : View(context) {
    enum class Kind { CALL, HANGUP, MIC, MIC_OFF, SPEAKER, EARPIECE, WIRED, BLUETOOTH, FLUID, FRACTAL }

    var kind = Kind.CALL
        set(v) { if (field != v) { field = v; invalidate() } }

    /** Spinning ring while connecting. */
    var busy = false
        set(v) { if (field != v) { field = v; invalidate() } }

    private companion object {
        /**
         * The fractal button's glyph: Douady's rabbit, the Julia set for c = -0.123 + 0.745i, drawn white with
         * a soft halo. It is computed once here (~10 ms) from the same iteration the fractal visual runs.
         */
        val julia: Bitmap by lazy {
            val n = 96
            val px = IntArray(n * n)
            for (y in 0 until n) {
                for (x in 0 until n) {
                    var zr = (x / (n - 1f) - 0.5f) * 2.5f
                    var zi = (0.5f - y / (n - 1f)) * 2.5f
                    var i = 0
                    while (i < 60 && zr * zr + zi * zi <= 16f) {
                        val t = zr * zr - zi * zi - 0.123f
                        zi = 2f * zr * zi + 0.745f
                        zr = t
                        i++
                    }
                    val alpha = if (i == 60) 255 else (255 * ((i - 3f) / 22f).coerceIn(0f, 1f) * 0.85f).toInt()
                    px[y * n + x] = (alpha shl 24) or 0xFFFFFF
                }
            }
            Bitmap.createBitmap(px, n, n, Bitmap.Config.ARGB_8888)
        }
    }

    private val bg = Paint(Paint.ANTI_ALIAS_FLAG)
    private val line = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE; strokeCap = Paint.Cap.ROUND; strokeJoin = Paint.Join.ROUND; color = Color.WHITE
    }
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.WHITE }
    private val bitmapPaint = Paint(Paint.ANTI_ALIAS_FLAG or Paint.FILTER_BITMAP_FLAG)
    private val path = Path()
    private val rect = RectF()
    private var pressedNow = false

    init {
        isClickable = true
        isFocusable = true
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> { pressedNow = true; invalidate() }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> { pressedNow = false; invalidate() }
        }
        return super.onTouchEvent(event)
    }

    override fun onDraw(c: Canvas) {
        val s = minOf(width, height).toFloat()
        c.save()
        c.translate((width - s) / 2, (height - s) / 2)
        bg.color = when (kind) {
            Kind.CALL -> 0xFF22C55E.toInt()
            Kind.HANGUP -> 0xFFEF4444.toInt()
            Kind.MIC_OFF -> 0xFFB45309.toInt()
            else -> 0x88202A38.toInt()
        }
        bg.alpha = (bg.alpha * (if (pressedNow) 0.7f else 1f)).toInt()
        c.drawCircle(s / 2, s / 2, s / 2, bg)
        line.strokeWidth = s * 0.075f
        glyph(c, s)
        if (busy) {
            line.strokeWidth = s * 0.05f
            rect.set(s * 0.04f, s * 0.04f, s * 0.96f, s * 0.96f)
            c.drawArc(rect, (System.currentTimeMillis() % 1000) * 0.36f, 90f, false, line)
            postInvalidateOnAnimation()
        }
        c.restore()
    }

    private fun glyph(c: Canvas, s: Float) {
        fun r(l: Float, t: Float, rr: Float, b: Float) = rect.set(l * s, t * s, rr * s, b * s)
        line.style = Paint.Style.STROKE
        when (kind) {
            Kind.CALL, Kind.HANGUP -> {
                c.save()
                if (kind == Kind.HANGUP) c.rotate(180f, s / 2, s * 0.52f)
                line.strokeWidth = s * 0.13f
                r(0.2f, 0.3f, 0.8f, 0.9f)
                c.drawArc(rect, 205f, 130f, false, line)
                c.drawLine(s * 0.27f, s * 0.47f, s * 0.27f, s * 0.58f, line)
                c.drawLine(s * 0.73f, s * 0.47f, s * 0.73f, s * 0.58f, line)
                c.restore()
            }
            Kind.MIC, Kind.MIC_OFF -> {
                r(0.4f, 0.22f, 0.6f, 0.56f); c.drawRoundRect(rect, s * 0.1f, s * 0.1f, line)
                r(0.31f, 0.38f, 0.69f, 0.68f); c.drawArc(rect, 0f, 180f, false, line)
                c.drawLine(s * 0.5f, s * 0.68f, s * 0.5f, s * 0.78f, line)
                c.drawLine(s * 0.4f, s * 0.78f, s * 0.6f, s * 0.78f, line)
                if (kind == Kind.MIC_OFF) c.drawLine(s * 0.27f, s * 0.25f, s * 0.73f, s * 0.80f, line)
            }
            Kind.SPEAKER -> {
                path.reset()
                path.moveTo(s * 0.28f, s * 0.42f); path.lineTo(s * 0.38f, s * 0.42f); path.lineTo(s * 0.52f, s * 0.30f)
                path.lineTo(s * 0.52f, s * 0.70f); path.lineTo(s * 0.38f, s * 0.58f); path.lineTo(s * 0.28f, s * 0.58f); path.close()
                line.strokeWidth = s * 0.05f; c.drawPath(path, line)
                line.strokeWidth = s * 0.06f
                r(0.40f, 0.38f, 0.70f, 0.62f); c.drawArc(rect, -50f, 100f, false, line)
                r(0.34f, 0.28f, 0.80f, 0.72f); c.drawArc(rect, -50f, 100f, false, line)
            }
            Kind.EARPIECE -> {
                r(0.36f, 0.2f, 0.64f, 0.8f); c.drawRoundRect(rect, s * 0.06f, s * 0.06f, line)
                c.drawLine(s * 0.46f, s * 0.3f, s * 0.54f, s * 0.3f, line)
            }
            Kind.WIRED -> {
                r(0.28f, 0.26f, 0.72f, 0.70f); c.drawArc(rect, 180f, 180f, false, line)
                r(0.25f, 0.48f, 0.37f, 0.68f); c.drawRoundRect(rect, s * 0.03f, s * 0.03f, fill)
                r(0.63f, 0.48f, 0.75f, 0.68f); c.drawRoundRect(rect, s * 0.03f, s * 0.03f, fill)
            }
            Kind.BLUETOOTH -> {
                path.reset()
                path.moveTo(s * 0.40f, s * 0.38f); path.lineTo(s * 0.62f, s * 0.62f); path.lineTo(s * 0.50f, s * 0.74f)
                path.lineTo(s * 0.50f, s * 0.26f); path.lineTo(s * 0.62f, s * 0.38f); path.lineTo(s * 0.40f, s * 0.62f)
                c.drawPath(path, line)
            }
            Kind.FLUID -> for (i in 0..2) {
                val y = s * (0.36f + i * 0.14f)
                path.reset(); path.moveTo(s * 0.26f, y)
                path.cubicTo(s * 0.38f, y - s * 0.1f, s * 0.46f, y + s * 0.1f, s * 0.5f, y)
                path.cubicTo(s * 0.54f, y - s * 0.1f, s * 0.62f, y + s * 0.1f, s * 0.74f, y)
                c.drawPath(path, line)
            }
            Kind.FRACTAL -> {
                r(0.10f, 0.10f, 0.90f, 0.90f)
                c.drawBitmap(julia, null, rect, bitmapPaint)
            }
        }
    }
}
