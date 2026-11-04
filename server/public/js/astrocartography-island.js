// First-party boot script for the astrocartography MapLibre island.
//
// Enhances the island mount rendered by `AstrocartographyMapIsland`
// (webapp/src/astrocartography.rs): reads the inline
// `astro-map-island-data` payload (one GeoJSON FeatureCollection per
// person), loads the vendored MapLibre GL JS bundle named by the mount's
// `data-module-src`, and draws each person's lines on the Vesta style named
// by `data-style-href`.
//
// Progressive enhancement in both directions. Without JavaScript the mount
// ships empty and CSS `:empty` hides it — the SVG map is the whole
// experience. If the map cannot start (no WebGL, a blocked module), the
// failure is stated in the mount and the SVG frame is left untouched. Only
// after a successful boot does the script flag the mount `data-live`, which
// the stylesheet reads to retire the static frame.
//
// Birth-data privacy: the payload holds only computed geometry for the
// specimen instant. The island makes exactly one kind of cross-origin
// request — raster tiles from tile.openstreetmap.org, which carry zoom/x/y
// coordinates and nothing else.
(function () {
  "use strict";
  if (window.__navAstroIsland) return;
  window.__navAstroIsland = true;

  var MOUNT_SELECTOR = ".astro-map-island[data-module-src]";
  var DATA_ID = "astro-map-island-data";

  // Line styling keyed by the SVG fallback's style indices
  // (.astro-map-line-0 … astro-map-line-3 in theme.css). MapLibre
  // dasharray units are multiples of line-width (2), so `8 3` reads as
  // [4, 1.5]. Colors are the Vesta bronze palette (views::brand_presentation,
  // palette "vesta"), darkest for the meridians, lightest for the DSC.
  var ANGLE_STYLES = [
    { name: "mc", color: "#63411f", dasharray: null },
    { name: "ic", color: "#774d25", dasharray: [4, 1.5] },
    { name: "asc", color: "#cb925a", dasharray: [1.5, 1.5] },
    { name: "dsc", color: "#d2a171", dasharray: [5, 1.5, 1, 1.5] }
  ];

  function angleStyle(angle) {
    for (var i = 0; i < ANGLE_STYLES.length; i += 1) {
      if (ANGLE_STYLES[i].name === angle) return ANGLE_STYLES[i];
    }
    return null;
  }

  function layerId(personId, body, angle) {
    return "astro-lines-" + personId + "-" + body + "-" + angle;
  }

  function readPayload() {
    var el = document.getElementById(DATA_ID);
    if (!el) return null;
    try {
      // Dioxus wraps the JSON object in hydration markers. Select the object
      // without rewriting its contents: comment-like text in JSON strings is
      // data, and this value is parsed as JSON, never inserted as HTML.
      var text = el.textContent;
      var start = text.indexOf("{");
      var end = text.lastIndexOf("}");
      if (start < 0 || end < start) return null;
      return JSON.parse(text.slice(start, end + 1));
    } catch (error) {
      console.error("astrocartography island: unreadable payload", error);
      return null;
    }
  }

  function statusMessage(mount, text) {
    var p = mount.querySelector(".astro-map-island__status");
    if (!p) {
      p = document.createElement("p");
      p.className = "astro-map-island__status";
      mount.appendChild(p);
    }
    p.textContent = text;
  }

  function showFailure(mount, text) {
    delete mount.dataset.live;
    statusMessage(mount, text);
    console.error("astrocartography island:", text);
  }

  function addLines(map, person) {
    var sourceId = "astro-lines-" + person.id;
    map.addSource(sourceId, { type: "geojson", data: person.geojson });
    (person.geojson.features || []).forEach(function (feature) {
      var style = angleStyle(feature.properties && feature.properties.angle);
      if (!style) return;
      var body = feature.properties.body;
      var layout = { "line-cap": "round", "line-join": "round" };
      var paint = { "line-color": style.color, "line-width": 2 };
      if (style.dasharray) paint["line-dasharray"] = style.dasharray;
      map.addLayer({
        id: layerId(person.id, body, style.name),
        type: "line",
        source: sourceId,
        filter: ["all", ["==", ["get", "body"], body], ["==", ["get", "angle"], style.name]],
        layout: layout,
        paint: paint
      });
    });
  }

  // All controls constrain the same layers. Recompute their intersection
  // whenever one changes so toggling an angle cannot reveal a hidden body.
  function applyVisibility(map, persons, visibility) {
    persons.forEach(function (person) {
      (person.geojson.features || []).forEach(function (feature) {
        var properties = feature.properties;
        if (!properties) return;
        var id = layerId(person.id, properties.body, properties.angle);
        if (!map.getLayer(id)) return;
        var visible = visibility.persons[person.id] !== false &&
          visibility.bodies[properties.body] !== false &&
          visibility.angles[properties.angle] !== false;
        map.setLayoutProperty(id, "visibility", visible ? "visible" : "none");
      });
    });
  }

  // The same body fieldset drives the Dioxus SVG and the plain-event island.
  function wireBodyToggles(map, persons, visibility) {
    var boxes = document.querySelectorAll('input[name="astro-map-body"]');
    Array.prototype.forEach.call(boxes, function (box) {
      var apply = function () {
        visibility.bodies[box.value] = box.checked;
        applyVisibility(map, persons, visibility);
      };
      box.addEventListener("change", apply);
      apply();
    });
  }

  function buildAngleControls(mount, map, persons, visibility) {
    var row = document.createElement("div");
    row.className = "astro-map-island__angles";
    ANGLE_STYLES.forEach(function (style) {
      var label = document.createElement("label");
      var box = document.createElement("input");
      box.type = "checkbox";
      box.name = "astro-map-island-angle";
      box.value = style.name;
      box.checked = true;
      box.addEventListener("change", function () {
        visibility.angles[style.name] = box.checked;
        applyVisibility(map, persons, visibility);
      });
      label.appendChild(box);
      label.appendChild(document.createTextNode(style.name.toUpperCase()));
      row.appendChild(label);
    });
    mount.appendChild(row);
  }

  function buildPersonControls(mount, map, persons, visibility) {
    if (persons.length < 2) return;
    var row = document.createElement("div");
    row.className = "astro-map-island__persons";
    persons.forEach(function (person) {
      var label = document.createElement("label");
      var box = document.createElement("input");
      box.type = "checkbox";
      box.name = "astro-map-island-person";
      box.value = person.id;
      box.checked = true;
      box.addEventListener("change", function () {
        visibility.persons[person.id] = box.checked;
        applyVisibility(map, persons, visibility);
      });
      label.appendChild(box);
      label.appendChild(document.createTextNode(person.label || person.id));
      row.appendChild(label);
    });
    mount.appendChild(row);
  }

  function boot(mount, payload, maplibregl) {
    statusMessage(mount, "Loading the interactive map…");
    var map;
    var failed = false;
    try {
      map = new maplibregl.Map({
        container: mount,
        style: mount.dataset.styleHref,
        center: [0, 15],
        zoom: 1.2,
        attributionControl: { compact: false }
      });
    } catch (error) {
      showFailure(mount, "The interactive map could not start. The static map below remains available.");
      return;
    }
    map.on("error", function () {
      // Tile or source errors degrade the basemap, not the lines; say so
      // once and leave the static map untouched as the alternative.
      failed = true;
      showFailure(mount, "Some map tiles failed to load. The static map below remains available.");
    });
    map.on("load", function () {
      if (!failed) statusMessage(mount, "");
      var persons = payload.persons || [];
      persons.forEach(function (person) { addLines(map, person); });
      var visibility = {
        bodies: Object.create(null),
        angles: Object.create(null),
        persons: Object.create(null)
      };
      buildPersonControls(mount, map, persons, visibility);
      buildAngleControls(mount, map, persons, visibility);
      wireBodyToggles(map, persons, visibility);
      if (!failed) mount.dataset.live = "true";
    });
  }

  function start() {
    var mounts = document.querySelectorAll(MOUNT_SELECTOR);
    if (!mounts.length) return;
    var payload = readPayload();
    if (!payload || !payload.persons || !payload.persons.length) {
      Array.prototype.forEach.call(mounts, function (mount) {
        showFailure(mount, "The interactive map payload is missing or unreadable. The static map below remains available.");
      });
      return;
    }
    Array.prototype.forEach.call(mounts, function (mount) {
      import(mount.dataset.moduleSrc)
        .then(function (maplibregl) { boot(mount, payload, maplibregl); })
        .catch(function (error) {
          showFailure(mount, "The interactive map module failed to load. The static map below remains available.");
          console.error("astrocartography island:", error);
        });
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", start);
  } else {
    start();
  }
})();
