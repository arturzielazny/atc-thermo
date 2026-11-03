/*
 * History chart: one line per device over a time span, with axes, gridlines on round
 * values and local times, and a crosshair that shows every device's value where the
 * finger is. Samples are reduced to their range per pixel column, so two days of eight
 * devices draw as fast as ten minutes.
 */
package dev.artur.thermo.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.artur.thermo.model.Metric
import dev.artur.thermo.model.Sample
import dev.artur.thermo.model.Source
import java.util.TimeZone
import kotlin.math.abs
import kotlin.math.ceil
import kotlin.math.floor
import kotlin.math.log10
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow

/** One device's line. [samples] are sorted by time and only read while drawing. */
class ChartSeries(val address: String, val name: String, val color: Color, val samples: List<Sample>)

/** Time steps for vertical gridlines, the smallest giving at most this many lines is used. */
private val TIME_STEPS_MIN = listOf(1, 2, 5, 10, 15, 30, 60, 120, 180, 360, 720, 1440)
private const val MAX_TIME_LINES = 5
/** The sensors resolve 0.01 °C and 0.01 %RH, but humidity varies by tenths. */
private val TOOLTIP_DECIMALS = mapOf(Metric.Temperature to 2, Metric.Humidity to 1, Metric.Rssi to 0)
/** Live samples further apart than this are not joined: reception dropped in between. */
private const val LIVE_GAP_MS = 6 * 60_000L
/** Log records are 30 min averages by default; neighbours that far apart still join. */
private const val LOG_GAP_MS = 65 * 60_000L

@Composable
fun HistoryChart(
    series: List<ChartSeries>,
    metric: Metric,
    from: Long,
    to: Long,
    /** Changes whenever samples were added, so the chart redraws. */
    revision: Long,
    modifier: Modifier = Modifier,
    points: Boolean = false,
    /** Shade what lies outside this time range, e.g. a calibration window. */
    window: Pair<Long, Long>? = null,
) {
    val measurer = rememberTextMeasurer()
    var cursor by remember { mutableStateOf<Float?>(null) }
    val colors = MaterialTheme.colorScheme
    val labelStyle = TextStyle(fontSize = 11.sp, color = colors.onSurfaceVariant)
    val grid = colors.outlineVariant.copy(alpha = 0.6f)
    val shade = colors.onSurface.copy(alpha = 0.12f)
    val tooltipBackground = colors.inverseSurface.copy(alpha = 0.92f)
    val tooltipText = colors.inverseOnSurface
    val cursorLine = colors.onSurface.copy(alpha = 0.6f)

    Canvas(
        modifier
            .pointerInput(Unit) {
                detectTapGestures(onPress = {
                    cursor = it.x
                    tryAwaitRelease()
                    cursor = null
                })
            }
            .pointerInput(Unit) {
                detectHorizontalDragGestures(
                    onDragStart = { cursor = it.x },
                    onDragEnd = { cursor = null },
                    onDragCancel = { cursor = null },
                    onHorizontalDrag = { change, _ -> cursor = change.position.x },
                )
            }
    ) {
        @Suppress("UNUSED_EXPRESSION") revision
        val leftPad = 44.dp.toPx()
        val bottomPad = 20.dp.toPx()
        val topPad = 6.dp.toPx()
        val plot = PlotArea(leftPad, topPad, size.width - leftPad - 4.dp.toPx(), size.height - topPad - bottomPad)
        if (plot.width <= 0 || plot.height <= 0 || to <= from) return@Canvas

        val visible = series.map { s -> s to visibleRange(s.samples, from, to) }
        var lo = Double.POSITIVE_INFINITY
        var hi = Double.NEGATIVE_INFINITY
        for ((s, range) in visible) for (i in range) {
            val v = metric.of(s.samples[i]) ?: continue
            lo = min(lo, v.toDouble())
            hi = max(hi, v.toDouble())
        }
        if (lo > hi) {
            drawText(measurer, "No ${metric.label.lowercase()} data in this span", Offset(plot.left + 8.dp.toPx(), plot.top + plot.height / 2 - 8.dp.toPx()), labelStyle)
            drawTimeAxis(measurer, labelStyle, grid, plot, from, to)
            return@Canvas
        }
        val pad = max((hi - lo) * 0.08, 0.0)
        if (hi - lo < metric.minSpan) {
            val mid = (hi + lo) / 2
            lo = mid - metric.minSpan / 2
            hi = mid + metric.minSpan / 2
        } else {
            lo -= pad
            hi += pad
        }
        val step = niceStep((hi - lo) / 4, metric.minGridStep)
        val yScale = YScale(lo, hi, plot)

        // Horizontal gridlines with labels.
        val decimals = max(0, -floor(log10(step)).toInt())
        var y = ceil(lo / step) * step
        while (y <= hi) {
            val py = yScale.of(y)
            drawLine(grid, Offset(plot.left, py), Offset(plot.right, py), 1f)
            val label = "%.${decimals}f".format(y)
            val layout = measurer.measure(label, labelStyle)
            drawText(layout, topLeft = Offset(plot.left - layout.size.width - 4.dp.toPx(), py - layout.size.height / 2))
            y += step
        }
        drawTimeAxis(measurer, labelStyle, grid, plot, from, to)

        window?.let { (start, end) ->
            val x0 = plot.xOf(start, from, to).coerceIn(plot.left, plot.right)
            val x1 = plot.xOf(end, from, to).coerceIn(plot.left, plot.right)
            if (x0 > plot.left) drawRect(shade, Offset(plot.left, plot.top), Size(x0 - plot.left, plot.height))
            if (x1 < plot.right) drawRect(shade, Offset(x1, plot.top), Size(plot.right - x1, plot.height))
            for (x in listOf(x0, x1)) drawLine(colors.primary, Offset(x, plot.top), Offset(x, plot.bottom), 2.dp.toPx())
        }

        val stroke = Stroke(width = 1.8.dp.toPx(), cap = StrokeCap.Round, join = StrokeJoin.Round)
        for ((s, range) in visible) {
            if (points) drawPoints(s, range, metric, plot, yScale, from, to)
            else drawPath(linePath(s, range, metric, plot, yScale, from, to), s.color, style = stroke)
        }

        cursor?.let { drawCursor(it, visible, metric, plot, from, to, measurer, tooltipBackground, tooltipText, cursorLine) }
    }
}

private class PlotArea(val left: Float, val top: Float, val width: Float, val height: Float) {
    val right get() = left + width
    val bottom get() = top + height
    fun xOf(t: Long, from: Long, to: Long) = left + width * ((t - from).toFloat() / (to - from))
    fun timeAt(x: Float, from: Long, to: Long) = from + ((x - left) / width * (to - from)).toLong()
}

private class YScale(val lo: Double, val hi: Double, val plot: PlotArea) {
    fun of(v: Double) = (plot.bottom - (v - lo) / (hi - lo) * plot.height).toFloat()
}

/** Indices of samples within `from..to`. */
private fun visibleRange(samples: List<Sample>, from: Long, to: Long): IntRange {
    fun firstAtOrAfter(t: Long): Int {
        var lo = 0
        var hi = samples.size
        while (lo < hi) {
            val mid = (lo + hi) ushr 1
            if (samples[mid].at < t) lo = mid + 1 else hi = mid
        }
        return lo
    }
    return firstAtOrAfter(from) until firstAtOrAfter(to + 1)
}

/** A line through every pixel column's first, lowest, highest and last value, broken at gaps. */
private fun linePath(s: ChartSeries, range: IntRange, metric: Metric, plot: PlotArea, y: YScale, from: Long, to: Long): Path {
    val path = Path()
    var previous: Sample? = null
    var column = Int.MIN_VALUE
    var colMin = 0f
    var colMax = 0f
    var colX = 0f
    fun flush() {
        if (column != Int.MIN_VALUE && colMax - colMin > 0.5f) {
            path.lineTo(colX, colMin)
            path.lineTo(colX, colMax)
        }
    }
    for (i in range) {
        val sample = s.samples[i]
        val v = metric.of(sample) ?: continue
        val x = plot.xOf(sample.at, from, to)
        val py = y.of(v.toDouble())
        val prev = previous
        val gap = if (prev == null) true else {
            val limit = if (sample.source == Source.Log || prev.source == Source.Log) LOG_GAP_MS else LIVE_GAP_MS
            sample.at - prev.at > limit
        }
        val c = x.toInt()
        if (gap) {
            flush()
            path.moveTo(x, py)
            column = c
            colMin = py; colMax = py; colX = x
        } else if (c != column) {
            flush()
            path.lineTo(x, py)
            column = c
            colMin = py; colMax = py; colX = x
        } else {
            colMin = min(colMin, py)
            colMax = max(colMax, py)
        }
        previous = sample
    }
    flush()
    return path
}

private fun DrawScope.drawPoints(s: ChartSeries, range: IntRange, metric: Metric, plot: PlotArea, y: YScale, from: Long, to: Long) {
    val radius = 1.6.dp.toPx()
    var lastX = -10f
    var lastY = -10f
    for (i in range) {
        val v = metric.of(s.samples[i]) ?: continue
        val x = plot.xOf(s.samples[i].at, from, to)
        val py = y.of(v.toDouble())
        // Skip points that would land on the previous one.
        if (abs(x - lastX) < radius && abs(py - lastY) < radius) continue
        drawCircle(s.color, radius, Offset(x, py))
        lastX = x
        lastY = py
    }
}

private fun DrawScope.drawTimeAxis(measurer: TextMeasurer, style: TextStyle, grid: Color, plot: PlotArea, from: Long, to: Long) {
    val spanMin = (to - from) / 60_000.0
    val stepMin = TIME_STEPS_MIN.firstOrNull { spanMin / it <= MAX_TIME_LINES } ?: TIME_STEPS_MIN.last()
    val step = stepMin * 60_000L
    val zone = TimeZone.getDefault()
    val offset = zone.getOffset(from).toLong()
    var t = Math.floorDiv(from + offset, step) * step + step - offset
    while (t < to) {
        val x = plot.xOf(t, from, to)
        drawLine(grid, Offset(x, plot.top), Offset(x, plot.bottom), 1f)
        val local = (t + zone.getOffset(t)).mod(86_400_000L)
        val label = if (local == 0L) formatMoment(t, 0).substringBefore(' ') else "%02d:%02d".format(local / 3_600_000, local / 60_000 % 60)
        val layout = measurer.measure(label, style)
        val lx = (x - layout.size.width / 2).coerceIn(plot.left, plot.right - layout.size.width)
        drawText(layout, topLeft = Offset(lx, plot.bottom + 3.dp.toPx()))
        t += step
    }
    drawLine(grid, Offset(plot.left, plot.bottom), Offset(plot.right, plot.bottom), 1.5f)
}

/** A vertical line at [x] and a box with the time and each device's nearest value. */
private fun DrawScope.drawCursor(
    x: Float,
    visible: List<Pair<ChartSeries, IntRange>>,
    metric: Metric,
    plot: PlotArea,
    from: Long,
    to: Long,
    measurer: TextMeasurer,
    background: Color,
    text: Color,
    line: Color,
) {
    val cx = x.coerceIn(plot.left, plot.right)
    val at = plot.timeAt(cx, from, to)
    drawLine(line, Offset(cx, plot.top), Offset(cx, plot.bottom), 1.dp.toPx())
    // Nearest sample within a few pixels' worth of time, or within the live gap.
    val tolerance = max((to - from) / plot.width * 12, 120_000f).toLong()
    val rows = visible.mapNotNull { (s, range) ->
        val nearest = range.minByOrNull { abs(s.samples[it].at - at) } ?: return@mapNotNull null
        val sample = s.samples[nearest]
        val v = metric.of(sample) ?: return@mapNotNull null
        if (abs(sample.at - at) > tolerance) null else Triple(s, v, sample.source)
    }
    val style = TextStyle(fontSize = 12.sp, color = text)
    val lines = buildList {
        add(measurer.measure(formatMoment(at, System.currentTimeMillis()), style.copy(fontWeight = FontWeight.Bold)))
        for ((s, v, source) in rows) {
            val suffix = if (source == Source.Log) " (log)" else ""
            add(measurer.measure("${s.name}  %.${TOOLTIP_DECIMALS.getValue(metric)}f ${metric.unit}$suffix".format(v), style))
        }
    }
    val pad = 6.dp.toPx()
    val dot = 10.dp.toPx()
    val width = lines.maxOf { it.size.width } + 2 * pad + dot
    val height = lines.sumOf { it.size.height } + 2 * pad
    val left = if (cx + 12.dp.toPx() + width < plot.right) cx + 12.dp.toPx() else max(plot.left, cx - 12.dp.toPx() - width)
    drawRoundRect(background, Offset(left, plot.top), Size(width, height), androidx.compose.ui.geometry.CornerRadius(pad))
    var ty = plot.top + pad
    lines.forEachIndexed { i, layout ->
        if (i > 0) drawCircle(rows[i - 1].first.color, dot / 3, Offset(left + pad + dot / 3, ty + layout.size.height / 2))
        drawText(layout, topLeft = Offset(left + pad + if (i > 0) dot else 0f, ty))
        ty += layout.size.height
    }
}

/** A round step (1, 2 or 5 × 10ⁿ) near [raw], at least [min]. */
fun niceStep(raw: Double, min: Double): Double {
    val wanted = max(raw, min)
    val magnitude = 10.0.pow(floor(log10(wanted)))
    val step = listOf(1.0, 2.0, 5.0, 10.0).map { it * magnitude }.first { it >= wanted * 0.999 }
    return max(step, min)
}
