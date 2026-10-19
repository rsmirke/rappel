# Image test fixtures

Small real images for `media/dimensions/tests.rs` and `media/color/tests.rs` (ADR 0063), each named by its size, made with Pillow 12.3 (Python 3, Ubuntu 26.04) by this script, run as `python3 -I make_images.py <this folder>`:

```python
import sys
from PIL import Image

out = sys.argv[1]

def picture(w, h):
    img = Image.new("RGB", (w, h))
    for x in range(w):
        for y in range(h):
            img.putpixel((x, y), (x * 20 % 256, y * 30 % 256, 128))
    return img

picture(13, 9).save(f"{out}/13x9.png")
picture(3, 2).save(f"{out}/3x2.jpg", quality=80)
picture(5, 4).save(f"{out}/5x4-progressive.jpg", quality=80, progressive=True)
exif = Image.Exif()
exif[0x010F] = "Rappel test"
picture(6, 3).save(f"{out}/6x3-exif.jpg", quality=80, exif=exif)
picture(7, 5).save(f"{out}/7x5-lossy.webp", quality=50)
picture(7, 5).save(f"{out}/7x5-lossless.webp", lossless=True)
frames = [picture(9, 6), picture(9, 6).rotate(180)]
frames[0].save(f"{out}/9x6-animated.webp", save_all=True, append_images=frames[1:], duration=100, loop=0)
frames[0].save(f"{out}/9x6-animated.gif", save_all=True, append_images=frames[1:], duration=100, loop=0)
picture(11, 7).convert("P").save(f"{out}/11x7.gif")
```

The three `4x3-icc` images carry Pillow's sRGB ICC profile (588 bytes), added the same way:

```python
from PIL import ImageCms
icc = ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes()
picture(4, 3).save(f"{out}/4x3-icc.png", icc_profile=icc)
picture(4, 3).save(f"{out}/4x3-icc.jpg", quality=80, icc_profile=icc)
picture(4, 3).save(f"{out}/4x3-icc.webp", quality=50, icc_profile=icc)
```
