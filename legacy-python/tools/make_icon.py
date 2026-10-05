"""生成 lanshare/web/icon.ico（窗口图标、托盘图标、exe 图标共用；改图标时运行一次）。"""
import os

from PIL import Image, ImageDraw

BLUE = (37, 99, 235, 255)
WHITE = (255, 255, 255, 255)
SIZE = 1024  # 先画大图再缩小，边缘更平滑


def draw():
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle([40, 40, SIZE - 40, SIZE - 40], radius=220, fill=BLUE)
    shaft_w, head_w, head_h = 120, 330, 250
    # 左边向上的箭头
    cx, top, bottom = 370, 210, 814
    d.polygon([(cx, top), (cx - head_w // 2, top + head_h), (cx + head_w // 2, top + head_h)], fill=WHITE)
    d.rectangle([cx - shaft_w // 2, top + head_h - 2, cx + shaft_w // 2, bottom], fill=WHITE)
    # 右边向下的箭头
    cx = SIZE - 370
    d.polygon([(cx, bottom), (cx - head_w // 2, bottom - head_h), (cx + head_w // 2, bottom - head_h)], fill=WHITE)
    d.rectangle([cx - shaft_w // 2, top, cx + shaft_w // 2, bottom - head_h + 2], fill=WHITE)
    return img


def main():
    out = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "lanshare", "web", "icon.ico")
    big = draw().resize((256, 256), Image.LANCZOS)
    big.save(out, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    print(out)


if __name__ == "__main__":
    main()
