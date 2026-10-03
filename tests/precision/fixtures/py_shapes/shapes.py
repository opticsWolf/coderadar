"""One call shape per function; a trailing arrow comment names the correct target."""

import os

from models import Manager, Serializer, make_manager, util
import models


def plain_name():
    return util(1)  # -> util


def builtin_call():
    return len([])  # -> builtin


def local_variable():
    m = Manager()  # -> Manager
    m.add(1)  # -> Manager.add
    return m


def factory_variable():
    m = make_manager()  # -> make_manager
    m.add(1)  # -> Manager.add
    return m


def parameter_annotation(m: Manager):
    return m.dump()  # -> Manager.dump


def module_attribute():
    return models.util(2)  # -> util


def module_class():
    return models.Manager()  # -> Manager


def class_static():
    return Serializer.make()  # -> Serializer.make


def class_classmethod():
    return Serializer.build()  # -> Serializer.build


def chained_attribute(m: Manager):
    return m.ser.serialize(1)  # -> Serializer.serialize


def call_then_method():
    return make_manager().add(1)  # -> Manager.add


def stdlib_path():
    return os.path.join("a", "b")  # -> external


def stdlib_chain():
    return os.path.join("a").upper()  # -> external


def child_inherited():
    c = models.Child()  # -> Child
    return c.add(1)  # -> Manager.add


def nested_function():
    def inner():
        return util(3)  # -> util
    return inner()  # -> ?
