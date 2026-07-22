"""devices — the hardware the doer layer depends on: motor serial, lidar,
camera, sound, GPIO, and the group memberships that gate access to them.
Existence + permission checks only — nothing is opened for writing.
"""
import glob
import grp
import os

from doctor.core import detail

NAME = "devices"
DESCRIPTION = "motor/lidar serial, camera, sound, GPIO devices + permissions"
DEFAULT, HEAVY, TIMEOUT_S = True, False, 10


def _in_group(name):
    try:
        return name in [g.gr_name for g in grp.getgrall() if os.getlogin() in g.gr_mem] \
            or grp.getgrgid(os.getgid()).gr_name == name
    except Exception:  # noqa: BLE001 — os.getlogin can fail under systemd
        try:
            import pwd
            user = pwd.getpwuid(os.getuid()).pw_name
            return any(user in g.gr_mem for g in grp.getgrall() if g.gr_name == name)
        except Exception:  # noqa: BLE001
            return False


def run(ctx):
    details = []
    # MOTOR serial: existence AND the ADR-0033 Tier-3 exclusivity contract
    # (#887 / AOU-ACTUATION-SERIAL-001). Unlike the lidar (a sensor, dialout
    # access is fine), the motor port is the ACTUATION boundary: loose perms
    # mean any process can drive the wheels below the checker — the stock
    # vendor rule ships it 0777. FAIL, with the tightening fix, never a
    # "join dialout" hint.
    motor = ctx["robot_env"].get("KIRRA_MOTOR_PORT", "/dev/myserial")
    if os.path.exists(motor):
        import sys as _sys
        _sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.dirname(
            os.path.abspath(__file__)))))
        try:
            import serial_exclusivity
            violations = serial_exclusivity.preflight(motor)
        except Exception as e:  # noqa: BLE001 — census must never crash the doctor
            violations = [f"exclusivity check errored: {e}"]
        if violations:
            details.append(detail(
                "motor serial exclusivity", "FAIL",
                f"{motor}: " + "; ".join(violations)[:200],
                fix="install robot/install/99-kirra-serial-exclusivity.rules "
                    "(owner=<consumer user>, MODE=0600) + udevadm trigger; "
                    "stop other openers (disable_vendor_autostart.sh)"))
        else:
            details.append(detail("motor serial exclusivity", "PASS",
                                  f"{motor} (owner+mode 0600, no other holder)"))
    else:
        details.append(detail("motor serial", "FAIL", f"{motor} missing",
                              fix="check the USB lead + vendor udev rules (capture_from_robot.sh)"))

    # Lidar: existence + plain rw (sensor — group access is acceptable).
    path = "/dev/ydlidar"
    if os.path.exists(path):
        rw = os.access(path, os.R_OK | os.W_OK)
        details.append(detail("lidar serial", "PASS" if rw else "WARN",
                              f"{path}{'' if rw else ' (no rw access — dialout group?)'}",
                              fix=None if rw else "sudo usermod -aG dialout $USER"))
    else:
        details.append(detail("lidar serial", "WARN", f"{path} missing",
                              fix="check the USB lead + vendor udev rules (capture_from_robot.sh)"))

    cams = glob.glob("/dev/video*")
    details.append(detail("camera", "PASS" if cams else "WARN",
                          ", ".join(cams[:4]) or "no /dev/video*",
                          fix=None if cams else "check the Astra USB lead"))
    snd = glob.glob("/dev/snd/pcm*")
    details.append(detail("sound devices", "PASS" if snd else "WARN",
                          f"{len(snd)} PCM device(s)" if snd else "no /dev/snd/pcm*",
                          fix=None if snd else "plug the USB mic/speaker — R2_VOICE_AUDIO_SETUP.md §0"))
    gpio = glob.glob("/dev/gpiochip*")
    details.append(detail("GPIO chardev", "PASS" if gpio else "WARN",
                          ", ".join(gpio) or "none"))
    for g in ("audio", "gpio", "dialout"):
        details.append(detail(f"group: {g}", "PASS" if _in_group(g) else "WARN",
                              "member" if _in_group(g) else "not a member",
                              fix=None if _in_group(g) else f"sudo usermod -aG {g} $USER (re-login)"))
    return {"details": details}
