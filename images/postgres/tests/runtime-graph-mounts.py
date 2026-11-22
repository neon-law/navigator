"""Compare complete Docker mount sets without trusting their presentation order."""

import json
import sys


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate object key")
        result[key] = value
    return result


def reject_constant(_value):
    raise ValueError("nonfinite JSON value")


def canonical(raw):
    mounts = json.loads(raw, object_pairs_hook=unique_object, parse_constant=reject_constant)
    if not isinstance(mounts, list) or not mounts:
        raise ValueError("nonempty mount array required")
    destinations = set()
    for mount in mounts:
        if not isinstance(mount, dict):
            raise ValueError("mount object required")
        destination = mount.get("Destination")
        if (not isinstance(destination, str) or not destination.startswith("/")
                or destination in destinations):
            raise ValueError("invalid or duplicate mount destination")
        if not isinstance(mount.get("Type"), str) or not mount["Type"]:
            raise ValueError("mount type required")
        if not isinstance(mount.get("RW"), bool):
            raise ValueError("boolean mount access required")
        for field in ("Source", "Name", "Driver", "Mode", "Propagation"):
            if field in mount and not isinstance(mount[field], str):
                raise ValueError("invalid mount string field")
        destinations.add(destination)
    # Retain every field, including unknown future fields. Only array/object order changes.
    return json.dumps(sorted(mounts, key=lambda mount: mount["Destination"]),
                      sort_keys=True, allow_nan=False, separators=(",", ":"))


if __name__ == "__main__":
    try:
        if len(sys.argv) != 3 or canonical(sys.argv[1]) != canonical(sys.argv[2]):
            sys.exit(1)
    except (ValueError, TypeError, RecursionError):
        sys.exit("Invalid mount inspection; retaining captured runtime role-proof ID.")
