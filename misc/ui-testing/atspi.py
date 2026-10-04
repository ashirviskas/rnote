#!/usr/bin/env python3
"""Drives the app started with run_ui.sh through the accessibility interface.

    atspi.py dump                      the widgets with a name or a role worth knowing
    atspi.py click ROLE NAME [INDEX]   press a button, toggle a switch or toggle button
    atspi.py type ROLE NAME TEXT       replace the text of an entry; NAME may be - for any
    atspi.py state ROLE NAME           showing / focused / checked of a widget

Roles are the ones `dump` prints: "button", "toggle button", "switch", "entry", "page tab", "list item", "label".
What it cannot do: press keys, click or drag on the canvas, activate a list row. For those, the code under test
needs a temporary hook.
"""
import sys

import gi

gi.require_version('Atspi', '2.0')
from gi.repository import Atspi


def app():
    desktop = Atspi.get_desktop(0)
    for i in range(desktop.get_child_count()):
        child = desktop.get_child_at_index(i)
        if child and child.get_name() == 'rnote':
            return child
    sys.exit('The app is not running, or not reachable through the accessibility bus.')


def nodes(node=None, depth=0):
    node = node or app()
    try:
        yield depth, node.get_role_name(), node.get_name() or '', node
        count = node.get_child_count()
    except Exception:
        return
    for i in range(min(count, 80)):
        child = node.get_child_at_index(i)
        if child is not None:
            yield from nodes(child, depth + 1)


def find(role, name):
    found = [n for _, r, nm, n in nodes() if r == role and (name == '-' or nm == name)]
    if not found:
        sys.exit(f'No {role} named "{name}".')
    return found


def act(node):
    action = node.get_action_iface()
    names = [action.get_action_name(i) for i in range(action.get_n_actions())] if action else []
    for wanted in ('click', 'activate', 'press', 'toggle'):
        if wanted in names:
            return action.do_action(names.index(wanted))
    sys.exit(f'The widget has no action to press it, only {names}.')


def main():
    command, args = sys.argv[1], sys.argv[2:]
    if command == 'dump':
        dull = ('filler', 'panel', 'grouping', 'unknown', 'separator')
        for depth, role, name, _ in nodes():
            if name or role not in dull:
                print(f'{depth:2} {role} | {name}')
    elif command == 'click':
        found = find(args[0], args[1])
        # A row with a switch has two widgets of that name, the one that can be pressed is the last
        print(act(found[int(args[2]) if len(args) > 2 else -1]))
    elif command == 'type':
        entry = find(args[0], args[1])[0]
        entry.get_editable_text_iface().delete_text(0, entry.get_text_iface().get_character_count())
        print(entry.get_editable_text_iface().insert_text(0, args[2], len(args[2].encode())))
    elif command == 'state':
        states = find(args[0], args[1])[-1].get_state_set()
        print({name: states.contains(getattr(Atspi.StateType, name.upper()))
               for name in ('showing', 'focused', 'checked')})
    else:
        sys.exit(__doc__)


if __name__ == '__main__':
    main()
