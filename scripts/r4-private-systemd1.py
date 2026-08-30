#!/usr/bin/python3
"""Minimal private systemd-manager seam for the real-logind capture only."""

import os
from pathlib import Path

import dbus
import dbus.mainloop.glib
import dbus.service
from gi.repository import GLib


MANAGER = "org.freedesktop.systemd1.Manager"
UNIT = "org.freedesktop.systemd1.Unit"
SCOPE = "org.freedesktop.systemd1.Scope"
PROPERTIES = "org.freedesktop.DBus.Properties"

try:
    marker = Path("/run/clipmesh-r4-private-logind").read_text()
except OSError:
    marker = None
if (
    os.environ.get("DBUS_SYSTEM_BUS_ADDRESS")
    != "unix:path=/run/dbus/system_bus_socket"
    or marker != "isolated\n"
):
    raise SystemExit("isolated_logind_capture_unavailable")


def unit_path(name):
    escaped = "".join(
        character if character.isalnum() else f"_{ord(character):02x}"
        for character in name
    )
    return f"/org/freedesktop/systemd1/unit/{escaped}"


class Unit(dbus.service.Object):
    def __init__(self, bus, name):
        self.name = name
        super().__init__(bus, unit_path(name))

    @dbus.service.method(PROPERTIES, in_signature="ss", out_signature="v")
    def Get(self, interface, name):
        values = self.GetAll(interface)
        if name not in values:
            raise dbus.exceptions.DBusException(
                "org.freedesktop.DBus.Error.UnknownProperty", name
            )
        return values[name]

    @dbus.service.method(PROPERTIES, in_signature="s", out_signature="a{sv}")
    def GetAll(self, interface):
        if interface != UNIT:
            return {}
        return {
            "Id": dbus.String(self.name),
            "LoadState": dbus.String("loaded"),
            "ActiveState": dbus.String("active"),
            "SubState": dbus.String("running"),
            "Job": dbus.Struct(
                (dbus.UInt32(0), dbus.ObjectPath("/")), signature="uo"
            ),
        }

    @dbus.service.method(SCOPE, in_signature="", out_signature="")
    def Abandon(self):
        return None


class Manager(dbus.service.Object):
    def __init__(self, bus):
        super().__init__(bus, "/org/freedesktop/systemd1")
        self.bus = bus
        self.units = {}
        self.next_job = 1

    def complete(self, unit_name):
        if unit_name not in self.units:
            self.units[unit_name] = Unit(self.bus, unit_name)
        job_id = self.next_job
        self.next_job += 1
        job_path = dbus.ObjectPath(f"/org/freedesktop/systemd1/job/{job_id}")
        GLib.idle_add(self.emit_done, job_id, job_path, unit_name)
        return job_path

    def emit_done(self, job_id, job_path, unit_name):
        self.JobRemoved(job_id, job_path, unit_name, "done")
        return False

    @dbus.service.method(MANAGER, in_signature="ss", out_signature="o")
    def StartUnit(self, name, mode):
        return self.complete(str(name))

    @dbus.service.method(MANAGER, in_signature="ss", out_signature="o")
    def StopUnit(self, name, mode):
        return self.complete(str(name))

    @dbus.service.method(
        MANAGER, in_signature="ssa(sv)a(sa(sv))", out_signature="o"
    )
    def StartTransientUnit(self, name, mode, properties, auxiliary):
        return self.complete(str(name))

    @dbus.service.method(MANAGER, in_signature="s", out_signature="o")
    def GetUnit(self, name):
        name = str(name)
        if name not in self.units:
            self.units[name] = Unit(self.bus, name)
        return dbus.ObjectPath(unit_path(name))

    @dbus.service.method(MANAGER, in_signature="sba(sv)", out_signature="")
    def SetUnitProperties(self, name, runtime, properties):
        return None

    @dbus.service.method(MANAGER, in_signature="", out_signature="")
    def Subscribe(self):
        return None

    @dbus.service.signal(MANAGER, signature="uoss")
    def JobRemoved(self, job_id, job_path, unit_name, result):
        return None


dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus = dbus.SystemBus()
name = dbus.service.BusName("org.freedesktop.systemd1", bus=bus)
manager = Manager(bus)
GLib.MainLoop().run()
