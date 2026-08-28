"""
Generates flat, top-down vector placeholder art for the farm sim,
matching the asset convention:
  assets/soil/{sandy,loamy,clay,tilled_overlay}.svg
  assets/crops/<id>/{sprout,growing,mature,dead}.svg

Style: simple flat shapes, no gradients/filters (keeps Inkscape's PNG
export predictable), viewBox 0 0 128 128, crop sprites have no
background rect (transparent), soil tiles are opaque.
"""

import os
import random

OUT = os.path.dirname(os.path.abspath(__file__))

def write(path, content):
    full = os.path.join(OUT, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w") as f:
        f.write(content.strip() + "\n")
    print("wrote", full)

def svg(body, viewbox="0 0 128 128"):
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{viewbox}" width="128" height="128">\n{body}\n</svg>'

# ---------------------------------------------------------------------
# Soil base textures (opaque) - flat color field + scattered speckles
# so the three soil types read as distinct even at small size.
# ---------------------------------------------------------------------

def speckles(n, colors, seed, rmin=1.0, rmax=2.6):
    rng = random.Random(seed)
    out = []
    for _ in range(n):
        x = rng.uniform(6, 122)
        y = rng.uniform(6, 122)
        r = rng.uniform(rmin, rmax)
        c = rng.choice(colors)
        out.append(f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r:.1f}" fill="{c}" opacity="0.55"/>')
    return "\n".join(out)

soil_sandy = svg(f'''
<rect width="128" height="128" fill="#d8c08a"/>
{speckles(70, ["#c9a96b", "#e8d9ae", "#b89760"], seed=1)}
''')
write("soil/sandy.svg", soil_sandy)

soil_loamy = svg(f'''
<rect width="128" height="128" fill="#4a3222"/>
{speckles(60, ["#3a2718", "#5c4130", "#2e1f14"], seed=2)}
''')
write("soil/loamy.svg", soil_loamy)

soil_clay = svg(f'''
<rect width="128" height="128" fill="#8a4a35"/>
{speckles(60, ["#733a28", "#a15c42", "#5f2f20"], seed=3)}
''')
write("soil/clay.svg", soil_clay)

# Tilled overlay: transparent background, dark furrow lines only.
# Meant to be layered on top of a soil base at render time.
furrows = []
for x in (14, 38, 62, 86, 110):
    furrows.append(
        f'<path d="M {x} 6 Q {x+6} 64 {x} 122" stroke="#2a1a10" '
        f'stroke-width="6" fill="none" opacity="0.5" stroke-linecap="round"/>'
    )
soil_tilled = svg("\n".join(furrows))
write("soil/tilled_overlay.svg", soil_tilled)

# ---------------------------------------------------------------------
# Crop sprites - transparent background, centered, simple flat shapes.
# Each crop gets a distinct silhouette per stage so they're readable
# as small tile icons.
# ---------------------------------------------------------------------

def leaf(cx, cy, length, width, angle, color, stroke="#1f3d14"):
    """A simple pointed-oval leaf as a path, rotated around (cx,cy)."""
    return (
        f'<g transform="translate({cx},{cy}) rotate({angle})">'
        f'<path d="M 0 0 Q {width/2} {-length/2} 0 {-length} Q {-width/2} {-length/2} 0 0 Z" '
        f'fill="{color}" stroke="{stroke}" stroke-width="1.5"/>'
        f'</g>'
    )

def stem(x1, y1, x2, y2, color="#3a6b1f", w=3):
    return f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{color}" stroke-width="{w}" stroke-linecap="round"/>'

# --- Tomato ---
write("crops/tomato/sprout.svg", svg(f'''
{stem(64, 96, 64, 74)}
{leaf(64, 74, 22, 14, -35, "#5fae3a")}
{leaf(64, 74, 22, 14, 35, "#5fae3a")}
'''))

write("crops/tomato/growing.svg", svg(f'''
{stem(64, 104, 64, 50)}
{leaf(64, 88, 30, 18, -40, "#4f9c30")}
{leaf(64, 88, 30, 18, 40, "#4f9c30")}
{leaf(64, 66, 28, 17, -25, "#5fae3a")}
{leaf(64, 66, 28, 17, 25, "#5fae3a")}
{leaf(64, 52, 24, 15, 0, "#6fbf47")}
'''))

_mature_leaves = "".join(
    leaf(64 + dx, 64 + dy, 34, 20, ang, "#4f9c30")
    for dx, dy, ang in [(-18,-6,-55),(18,-6,55),(0,-24,0),(-22,14,-110),(22,14,110),(-6,24,-160),(6,24,160)]
)
_tomatoes = "".join(
    f'<circle cx="{64+dx}" cy="{64+dy}" r="7" fill="#d43c2c" stroke="#7a1f14" stroke-width="1"/>'
    for dx, dy in [(-12,-2),(10,6),(-2,16),(14,-14),(0,-18)]
)
write("crops/tomato/mature.svg", svg(f'''
{_mature_leaves}
{_tomatoes}
'''))

write("crops/tomato/dead.svg", svg(f'''
{stem(64, 108, 60, 60, color="#6b5942", w=4)}
{stem(60, 60, 40, 40, color="#6b5942", w=3)}
{stem(60, 60, 78, 44, color="#6b5942", w=3)}
{leaf(40, 40, 18, 10, -50, "#8a7a58", stroke="#4a3f2a")}
{leaf(78, 44, 18, 10, 40, "#8a7a58", stroke="#4a3f2a")}
{leaf(64, 60, 16, 9, 10, "#7a6a4c", stroke="#4a3f2a")}
'''))

# --- Wheat ---
def blade(x, y, h, lean, color):
    tip_x = x + lean
    return f'<path d="M {x} {y} Q {x + lean*0.5} {y - h*0.6} {tip_x} {y-h}" stroke="{color}" stroke-width="3" fill="none" stroke-linecap="round"/>'

write("crops/wheat/sprout.svg", svg("\n".join(
    blade(x, 106, 26, lean, "#5fae3a")
    for x, lean in [(56,-4),(64,2),(72,6)]
)))

write("crops/wheat/growing.svg", svg("\n".join(
    blade(x, 108, 46, lean, "#4f9c30")
    for x, lean in [(46,-8),(54,-3),(62,2),(70,6),(78,10),(86,4)]
)))

_wheat_stalks = "\n".join(
    blade(x, 110, 66, lean, "#c9a227")
    for x, lean in [(38,-10),(48,-5),(58,0),(68,4),(78,8),(88,3),(98,-2)]
)
_wheat_heads = "".join(
    f'<ellipse cx="{x+lean}" cy="{110-66}" rx="4" ry="9" fill="#d9b93a" stroke="#8a6f1a" stroke-width="1"/>'
    for x, lean in [(38,-10),(48,-5),(58,0),(68,4),(78,8),(88,3),(98,-2)]
)
write("crops/wheat/mature.svg", svg(f'''
{_wheat_stalks}
{_wheat_heads}
'''))

write("crops/wheat/dead.svg", svg("\n".join(
    f'<path d="M {x} 100 Q {x+lean} 90 {x+lean*2} 96" stroke="#7a6a45" stroke-width="3" fill="none" stroke-linecap="round"/>'
    for x, lean in [(40,20),(56,18),(72,16),(88,14)]
)))

# --- Carrot ---
write("crops/carrot/sprout.svg", svg(f'''
{stem(64, 94, 64, 78)}
{leaf(64, 78, 18, 6, -20, "#5fae3a")}
{leaf(64, 78, 18, 6, 0, "#6fbf47")}
{leaf(64, 78, 18, 6, 20, "#5fae3a")}
'''))

_carrot_growing_leaves = "".join(
    leaf(64, 70, 26, 8, ang, "#4f9c30")
    for ang in (-30, -10, 10, 30, -50, 50)
)
write("crops/carrot/growing.svg", svg(_carrot_growing_leaves))

_carrot_mature_leaves = "".join(
    leaf(64, 60, 34, 9, ang, "#4f9c30")
    for ang in (-60,-40,-20,0,20,40,60,-80,80)
)
write("crops/carrot/mature.svg", svg(f'''
{_carrot_mature_leaves}
<path d="M 58 78 L 64 100 L 70 78 Z" fill="#e8792a" stroke="#a3501a" stroke-width="1"/>
'''))

write("crops/carrot/dead.svg", svg(f'''
{stem(64, 96, 64, 76, color="#7a6a4c", w=3)}
{leaf(64, 76, 22, 7, -25, "#8a7a58", stroke="#4a3f2a")}
{leaf(64, 76, 22, 7, 5, "#7a6a4c", stroke="#4a3f2a")}
{leaf(64, 76, 22, 7, 30, "#8a7a58", stroke="#4a3f2a")}
'''))

# --- Green Bean ---
write("crops/green_bean/sprout.svg", svg(f'''
{stem(64, 96, 64, 76)}
<ellipse cx="56" cy="72" rx="10" ry="7" fill="#5fae3a" stroke="#1f3d14" stroke-width="1.5"/>
<ellipse cx="72" cy="72" rx="10" ry="7" fill="#5fae3a" stroke="#1f3d14" stroke-width="1.5"/>
'''))

_bean_growing_vine = f'<path d="M 64 106 Q 48 84 64 68 Q 80 52 64 40" stroke="#3a6b1f" stroke-width="3" fill="none"/>'
_bean_growing_leaves = "".join(
    leaf(x, y, 20, 13, ang, "#4f9c30")
    for x, y, ang in [(52,86,-40),(76,76,40),(56,58,-30),(74,48,30)]
)
write("crops/green_bean/growing.svg", svg(f'''
{_bean_growing_vine}
{_bean_growing_leaves}
'''))

_bean_vine = f'<path d="M 64 110 Q 42 90 60 70 Q 82 50 62 30 Q 46 16 60 6" stroke="#3a6b1f" stroke-width="3" fill="none"/>'
_bean_leaves = "".join(
    leaf(x, y, 22, 14, ang, "#4f9c30")
    for x, y, ang in [(46,88,-50),(78,74,45),(52,54,-35),(74,38,35),(50,18,-40)]
)
_bean_pods = "".join(
    f'<path d="M {x} {y} Q {x+dx} {y+8} {x} {y+16} Q {x-dx} {y+8} {x} {y} Z" fill="#3f8a2a" stroke="#1f4a14" stroke-width="1"/>'
    for x, y, dx in [(58,66,5),(80,58,4),(50,40,5),(70,26,4)]
)
write("crops/green_bean/mature.svg", svg(f'''
{_bean_vine}
{_bean_leaves}
{_bean_pods}
'''))

write("crops/green_bean/dead.svg", svg(f'''
<path d="M 64 108 Q 46 92 60 74 Q 78 58 62 42" stroke="#7a6a4c" stroke-width="3" fill="none"/>
{leaf(48, 88, 16, 10, -45, "#8a7a58", stroke="#4a3f2a")}
{leaf(74, 70, 16, 10, 40, "#7a6a4c", stroke="#4a3f2a")}
{leaf(52, 52, 14, 9, -30, "#8a7a58", stroke="#4a3f2a")}
'''))

print("done")
