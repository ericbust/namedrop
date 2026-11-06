"""Find text ink in a frame and measure its cap height, for focal rings and legibility."""
import numpy as np


def ink_mask(gray, threshold=60):
    """Pixels that differ from the crop's most common value: text on a light or a dark ground."""
    pixels = np.asarray(gray, dtype=np.int16)
    background = np.bincount(pixels.ravel()).argmax()
    return np.abs(pixels - background) > threshold


def box_any(mask, radius):
    """For each pixel, whether any pixel within radius is set, or with all=True every pixel."""
    padded = np.pad(mask.astype(np.int32), radius + 1)
    total = padded.cumsum(0).cumsum(1)
    size = 2 * radius + 1
    window = total[size:, size:] - total[:-size, size:] - total[size:, :-size] + total[:-size, :-size]
    return window[:mask.shape[0], :mask.shape[1]]


def without_blocks(mask, stroke=3):
    """The mask without what is not text: solid shapes thicker than a glyph stroke, such as
    avatars or a cursor, and rules or borders that cross nearly the whole crop. Either would
    join text lines into one or count as a line of its own."""
    area = (2 * stroke + 1) ** 2
    solid = box_any(mask, stroke) == area
    mask = mask & ~(box_any(solid, 2 * stroke + 2) > 0)
    mask[:, longest_runs(mask.T) > 0.9 * mask.shape[0]] = False
    mask[longest_runs(mask) > 0.7 * mask.shape[1], :] = False
    return mask


def longest_runs(mask):
    """The longest unbroken run of ink in each row. A rule is one long run; bold text has gaps."""
    runs = []
    for row in mask:
        edges = np.diff(np.concatenate(([0], row.astype(np.int8), [0])))
        starts, stops = np.nonzero(edges == 1)[0], np.nonzero(edges == -1)[0]
        runs.append((stops - starts).max() if len(starts) else 0)
    return np.array(runs)


def line_caps(mask, gap=2, smallest=10):
    """Cap height in pixels of each text line in a mask, top to bottom.

    A line is a run of rows with ink, once blocks are removed. Its baseline is the most common lowest ink row over its
    columns, and its cap line is the 3rd percentile of their highest ink rows, so a few
    ascenders or descenders do not move either."""
    mask = without_blocks(mask)
    rows = np.nonzero(mask.any(axis=1))[0]
    if not len(rows):
        return []
    bands, begin = [], rows[0]
    for previous, row in zip(rows, rows[1:]):
        if row - previous > gap:
            bands.append((begin, previous))
            begin = row
    bands.append((begin, rows[-1]))
    caps = []
    for top, bottom in bands:
        band = mask[top:bottom + 1]
        columns = np.nonzero(band.any(axis=0))[0]
        if bottom - top + 1 < smallest or len(columns) < 2 * (bottom - top + 1):
            continue
        tops = band[:, columns].argmax(axis=0)
        bottoms = band.shape[0] - 1 - band[::-1, columns].argmax(axis=0)
        baseline = np.bincount(bottoms).argmax()
        caps.append(int(baseline - np.percentile(tops, 3) + 1))
    return caps
