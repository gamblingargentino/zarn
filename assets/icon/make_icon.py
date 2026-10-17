from PIL import Image, ImageDraw, ImageFilter

S = 1024
Z = [(240, 260), (784, 260), (784, 376), (450, 648), (784, 648), (784, 764),
     (240, 764), (240, 648), (574, 376), (240, 376)]
TOP, BOTTOM = (176, 106, 255), (110, 30, 200)
BG_TOP, BG_BOTTOM = (31, 22, 52), (14, 10, 26)


def vertical(size, top, bottom):
    strip = Image.new("RGB", (1, size))
    for y in range(size):
        t = y / (size - 1)
        strip.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))
    return strip.resize((size, size))


def rounded_mask(inset, radius):
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle((inset, inset, S - inset, S - inset), radius, fill=255)
    return mask


canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))
canvas.paste(vertical(S, BG_TOP, BG_BOTTOM), (0, 0), rounded_mask(24, 230))

glow = Image.new("L", (S, S), 0)
ImageDraw.Draw(glow).polygon(Z, fill=200)
glow = glow.filter(ImageFilter.GaussianBlur(46))
canvas.paste(Image.new("RGBA", (S, S), (138, 43, 226, 255)), (0, 0), glow.point(lambda v: v * 0.55))

shadow = Image.new("L", (S, S), 0)
ImageDraw.Draw(shadow).polygon([(x + 0, y + 26) for x, y in Z], fill=120)
shadow = shadow.filter(ImageFilter.GaussianBlur(14))
canvas.paste(Image.new("RGBA", (S, S), (0, 0, 0, 255)), (0, 0), shadow)

mark = Image.new("L", (S, S), 0)
ImageDraw.Draw(mark).polygon(Z, fill=255)
body = vertical(S, TOP, BOTTOM).convert("RGBA")
canvas.paste(body, (0, 0), mark)

rim = Image.new("L", (S, S), 0)
ImageDraw.Draw(rim).rounded_rectangle((24, 24, S - 24, S - 24), 230, outline=255, width=6)
canvas.paste(Image.new("RGBA", (S, S), (255, 255, 255, 38)), (0, 0), rim)

canvas.save("zarn.png")
sizes = [16, 24, 32, 48, 64, 128, 256]
canvas.save("zarn.ico", sizes=[(s, s) for s in sizes])
canvas.resize((256, 256), Image.LANCZOS).save("zarn-256.png")

points = " ".join(f"{x},{y}" for x, y in Z)
open("zarn.svg", "w").write(f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {S} {S}">
<defs>
<linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="rgb{BG_TOP}"/><stop offset="1" stop-color="rgb{BG_BOTTOM}"/></linearGradient>
<linearGradient id="z" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="rgb{TOP}"/><stop offset="1" stop-color="rgb{BOTTOM}"/></linearGradient>
</defs>
<rect x="24" y="24" width="976" height="976" rx="230" fill="url(#bg)"/>
<polygon points="{points}" fill="url(#z)"/>
</svg>
''')
