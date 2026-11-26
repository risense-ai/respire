"""Rebuild PNG and MP4 from a bounded stream of fixed-size RGB frames."""
from pathlib import Path
import subprocess
import sys
import tempfile


def convert(source, destination, locale, ext):
    name = f"{locale}.{ext}"
    still = ext == "png"
    frames = 1 if still else 144
    frame_bytes = 1024 * 768 * 3
    base = ["ffmpeg", "-nostdin", "-v", "error", "-xerror", "-threads", "1"]
    decoder = base + ["-max_alloc", "67108864", "-protocol_whitelist", "file",
                      "-f", "png_pipe" if still else "mov"]
    if not still:
        decoder += ["-enable_drefs", "0", "-use_absolute_path", "0"]
    decoder += ["-i", str(source / name), "-map", "0:v:0", "-an", "-sn", "-dn",
                "-vf", "fps=12,scale=1024:768" if not still else "scale=1024:768",
                "-frames:v", str(frames), "-threads", "1", "-pix_fmt", "rgb24", "-f", "rawvideo", "pipe:1"]
    encoder = base + ["-f", "rawvideo", "-pixel_format", "rgb24", "-video_size", "1024x768",
                      "-framerate", "12", "-i", "pipe:0", "-map_metadata", "-1", "-threads", "1"]
    encoder += ["-frames:v", "1", "-c:v", "png"] if still else [
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-movflags", "+faststart"]
    with tempfile.TemporaryFile() as pixels, subprocess.Popen(decoder, stdout=subprocess.PIPE) as proc:
        try:
            count = 0
            while chunk := proc.stdout.read(65536):
                count += len(chunk)
                if count > frames * frame_bytes:
                    raise ValueError("excess decoded frames")
                pixels.write(chunk)
            if proc.wait(timeout=5) or not count or count % frame_bytes:
                raise ValueError("incomplete decoded frame")
        finally:
            proc.kill()
        pixels.seek(0)
        subprocess.run(encoder + [str(destination / name)], stdin=pixels, timeout=45, check=True)


if __name__ == "__main__":
    source, destination = map(Path, sys.argv[1:])
    for locale in ("en", "zh"):
        for ext in ("png", "mp4"):
            convert(source, destination, locale, ext)
