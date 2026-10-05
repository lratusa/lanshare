"""PyInstaller 打包入口（无控制台窗口）。出了意外就写日志并弹窗，不能悄无声息地闪退。"""
import os
import sys
import time
import traceback

from lanshare.desktop import show_error
from lanshare.main import data_dir, main

if __name__ == "__main__":
    try:
        code = main()
    except Exception:
        detail = traceback.format_exc()
        log_path = os.path.join(data_dir(), "lanshare.log")
        try:
            os.makedirs(os.path.dirname(log_path), exist_ok=True)
            with open(log_path, "a", encoding="utf-8") as f:
                f.write("%s 未处理的错误：\n%s\n" % (time.strftime("%Y-%m-%d %H:%M:%S"), detail))
        except OSError:
            pass
        show_error("局域网快传出错了：\n\n%s\n详细信息在：%s" % (detail.strip().splitlines()[-1], log_path))
        code = 1
    sys.exit(code)
