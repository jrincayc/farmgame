Soil definitions: one JSON file per soil type (see design doc section 6.1).
Adding a soil type means adding a JSON file here plus its sprite PNG
(path given by the "sprite" field) - no code changes.

Fields: wilting_point / field_capacity / saturation are volumetric
fractions; drainage_rate is the fraction of water above field capacity
lost per day; evaporation_mm_per_day is defined at 25 C; texture
(0 coarse .. 1 fine) orders soils for world generation.

The PNGs (sandy.png, loamy.png, clay.png, tilled_overlay.png) are
generated from the SVGs here: run assets/svg_to_png.sh (needs Inkscape).
