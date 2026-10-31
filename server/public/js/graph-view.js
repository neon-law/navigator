// Mount the relationship-graph views (project, lawyer, firm) with the
// vendored Sigma.js renderer and Graphology core.
//
// The graph pages render `.graph-view[data-endpoint]` mounts whose data
// attributes name everything this script may touch: the endpoint URL (with
// the depth already resolved server-side, so the island can only fetch what
// the server rendered) and, per vertex kind, a click-through href template
// the viewer's tier may open (`{id}` is the placeholder). The island fetches
// the endpoint JSON same-origin — the `/app/api/graph/*` handlers keep the
// depth cap and tenancy — lays the neighborhood out, and draws it. Everything
// stays same-origin, so the page's `script-src 'self'` and `connect-src`
// (via `default-src 'self'`) need nothing new.
//
// Pure helpers (colors, edge styling, graph building, layout) are exported
// under `module.exports` when the file is loaded by Node, so the workspace's
// headless smoke can drive the fetch→build→layout pipeline without a browser.
(function () {
  "use strict";

  // Vertex-kind palette. The legend chips in `webapp/src/graph_views.rs` and
  // the `.graph-legend__chip--*` colors in `theme.css` restate these — keep
  // the three in step.
  var NODE_COLORS = {
    Person: "#3b6ea5",
    Entity: "#7d5ba6",
    Project: "#3a7d44",
    Firm: "#b5651d",
  };
  var NODE_FALLBACK_COLOR = "#6f7680";
  // An adverse edge is the conflict signal; it draws red and heavy. A
  // RELATIONSHIP of any other kind draws slate, a structural tie (role,
  // participation, membership, handling) draws light gray.
  var ADVERSE_COLOR = "#c1121f";
  var RELATION_COLOR = "#5f6f80";
  var TIE_COLOR = "#9aa3ab";
  var SELECT_COLOR = "#1d3557";
  var DIM_COLOR = "#d5d9de";

  function nodeColor(kind) {
    return NODE_COLORS[kind] || NODE_FALLBACK_COLOR;
  }

  // One edge's drawn shape: color by kind, weight by confidence for the
  // relationship edges the conflict check reads, arrows for the directed
  // ties, plain lines for the structural ones.
  function edgeStyle(edge) {
    var type = edge.type;
    if (type === "RELATIONSHIP") {
      var adverse = edge.kind === "adverse_to";
      var confidence = typeof edge.confidence_pct === "number" ? edge.confidence_pct : 0;
      return {
        color: adverse ? ADVERSE_COLOR : RELATION_COLOR,
        size: adverse ? 2 + confidence / 25 : 1 + confidence / 50,
        edgeType: "arrow",
      };
    }
    return { color: TIE_COLOR, size: 1, edgeType: "line" };
  }

  // The tooltip line for one edge: the relationship kind, the open-ended role
  // for the structural ties, and the confidence + provenance the conflict
  // readers carry.
  function edgeTooltip(edge) {
    var parts = [];
    if (edge.kind) parts.push(edge.kind);
    if (edge.role) parts.push("role: " + edge.role);
    if (typeof edge.confidence_pct === "number") {
      parts.push("confidence " + edge.confidence_pct + "%");
    }
    if (edge.source_kind) parts.push("source: " + edge.source_kind);
    return parts.join(" · ");
  }

  // Build the graphology Graph from one `{nodes, edges}` payload. Tolerant of
  // the endpoint's optional fields; drops edges whose endpoints did not come
  // back rather than asking the renderer to draw into the void.
  function buildGraph(Graph, payload) {
    var graph = new Graph({ multi: true, type: "directed", allowSelfLoops: true });
    var nodes = (payload && payload.nodes) || [];
    for (var i = 0; i < nodes.length; i++) {
      var node = nodes[i];
      graph.addNode(node.id, {
        label: node.name || node.id,
        kind: node.kind,
        x: 0,
        y: 0,
        size: 3,
        color: nodeColor(node.kind),
      });
    }
    var edges = (payload && payload.edges) || [];
    for (var j = 0; j < edges.length; j++) {
      var edge = edges[j];
      if (!graph.hasNode(edge.source) || !graph.hasNode(edge.target)) continue;
      if (graph.hasEdge(edge.source, edge.target)) continue;
      var style = edgeStyle(edge);
      graph.addEdgeWithKey(edge.source + "\u2192" + edge.target, edge.source, edge.target, {
        color: style.color,
        size: style.size,
        type: style.edgeType,
        relKind: edge.kind || "",
        tooltip: edgeTooltip(edge),
        hoverLabel: edgeTooltip(edge),
      });
    }
    return graph;
  }

  // Deterministic seed placement: one ring per vertex kind, the seed of the
  // view centered by kind order. Then a bounded Fruchterman-Reingold-style
  // relaxation — repulsion between every pair, attraction along edges, one
  // cooling factor. O(n²) per iteration with a small, depth-capped n, so no
  // worker and no addon is needed.
  function layout(graph, iterations) {
    var rings = { Person: 0, Entity: 1, Project: 2, Firm: 3 };
    var counts = {};
    var nodes = graph.nodes();
    var total = nodes.length || 1;
    var radius = Math.max(1, Math.sqrt(total) * 2);
    for (var i = 0; i < nodes.length; i++) {
      var kind = graph.getNodeAttribute(nodes[i], "kind");
      var ring = rings[kind] === undefined ? 4 : rings[kind];
      var seen = counts[ring] || 0;
      counts[ring] = seen + 1;
      var angle = (2 * Math.PI * seen) / Math.max(3, ring + 2) + ring * 0.9;
      var r = radius * (0.5 + ring * 0.5);
      graph.setNodeAttribute(nodes[i], "x", r * Math.cos(angle));
      graph.setNodeAttribute(nodes[i], "y", r * Math.sin(angle));
    }
    var iters = iterations || 80;
    for (var step = 0; step < iters; step++) {
      var cooling = 1 - step / iters;
      var displacement = {};
      for (var a = 0; a < nodes.length; a++) {
        displacement[nodes[a]] = { x: 0, y: 0 };
      }
      for (var p = 0; p < nodes.length; p++) {
        for (var q = p + 1; q < nodes.length; q++) {
          var u = nodes[p];
          var v = nodes[q];
          var dx = graph.getNodeAttribute(u, "x") - graph.getNodeAttribute(v, "x");
          var dy = graph.getNodeAttribute(u, "y") - graph.getNodeAttribute(v, "y");
          var dist2 = dx * dx + dy * dy || 0.01;
          var force = (radius * radius) / dist2;
          var fx = (dx / Math.sqrt(dist2)) * force;
          var fy = (dy / Math.sqrt(dist2)) * force;
          displacement[u].x += fx;
          displacement[u].y += fy;
          displacement[v].x -= fx;
          displacement[v].y -= fy;
        }
      }
      graph.forEachEdge(function (edge, attributes, source, target) {
        var dx = graph.getNodeAttribute(target, "x") - graph.getNodeAttribute(source, "x");
        var dy = graph.getNodeAttribute(target, "y") - graph.getNodeAttribute(source, "y");
        var dist = Math.sqrt(dx * dx + dy * dy) || 0.01;
        var force = (dist * dist) / (radius * 4);
        var fx = (dx / dist) * force;
        var fy = (dy / dist) * force;
        displacement[source].x -= fx;
        displacement[source].y -= fy;
        displacement[target].x += fx;
        displacement[target].y += fy;
      });
      for (var n = 0; n < nodes.length; n++) {
        var id = nodes[n];
        var d = displacement[id];
        var len = Math.sqrt(d.x * d.x + d.y * d.y) || 0.01;
        var cap = cooling * 0.5;
        var scaleX = (d.x / len) * Math.min(len, cap);
        var scaleY = (d.y / len) * Math.min(len, cap);
        graph.setNodeAttribute(id, "x", graph.getNodeAttribute(id, "x") + scaleX);
        graph.setNodeAttribute(id, "y", graph.getNodeAttribute(id, "y") + scaleY);
      }
    }
    return graph;
  }

  function loadScript(src) {
    return new Promise(function (resolve, reject) {
      var existing = document.querySelector('script[src="' + src + '"]');
      if (existing) {
        if (existing.dataset.loaded) return resolve();
        existing.addEventListener("load", function () {
          resolve();
        });
        existing.addEventListener("error", function () {
          reject(new Error("failed to load " + src));
        });
        return;
      }
      var script = document.createElement("script");
      script.src = src;
      script.defer = true;
      script.addEventListener("load", function () {
        script.dataset.loaded = "1";
        resolve();
      });
      script.addEventListener("error", function () {
        reject(new Error("failed to load " + src));
      });
      document.head.appendChild(script);
    });
  }

  function setStatus(mount, text, isError) {
    var status = mount.querySelector(".graph-view__status");
    if (!status) {
      status = document.createElement("p");
      status.className = "muted graph-view__status";
      mount.appendChild(status);
    }
    status.textContent = text;
    status.hidden = !text;
    status.classList.toggle("graph-view__status--error", Boolean(isError));
  }

  function hrefTemplate(mount, kind) {
    var map = {
      Person: mount.dataset.hrefPerson,
      Firm: mount.dataset.hrefFirm,
      Entity: mount.dataset.hrefEntity,
    };
    return map[kind] || "";
  }

  async function mountViewer(mount, Graph, Sigma) {
    if (mount.dataset.graphMounted) return;
    mount.dataset.graphMounted = "1";
    var endpoint = mount.dataset.endpoint;
    if (!endpoint) {
      setStatus(mount, "The graph view is missing its endpoint.", true);
      return;
    }
    var payload;
    try {
      var response = await fetch(endpoint, { credentials: "same-origin" });
      if (!response.ok) {
        setStatus(
          mount,
          response.status === 404
            ? "This graph is not available for your view."
            : "The graph could not be loaded. Try again.",
          true
        );
        return;
      }
      payload = await response.json();
    } catch (error) {
      setStatus(mount, "The graph could not be loaded. Try again.", true);
      return;
    }
    if (!payload.nodes || payload.nodes.length === 0) {
      setStatus(mount, "No relationships recorded yet.");
      return;
    }

    var graph = layout(buildGraph(Graph, payload));

    // Degree drives node size after the fact: hubs — the conflict-relevant
    // counterparts — read bigger without any server round trip.
    graph.forEachNode(function (node) {
      graph.setNodeAttribute(node, "size", 3 + 2 * Math.min(graph.degree(node), 5));
    });

    var selectedNode = null;

    var renderer = new Sigma(graph, mount, {
      allowInvalidContainer: true,
      defaultEdgeType: "line",
      edgeProgramClasses: { arrow: Sigma.EdgeArrowProgram || Sigma.EdgeLineProgram },
      labelRenderedSizeThreshold: 0.1,
      nodeReducer: function (node, data) {
        var rendered = Object.assign({}, data);
        if (selectedNode && selectedNode !== node) {
          var neighbor = graph.areNeighbors(selectedNode, node);
          rendered.color = neighbor ? SELECT_COLOR : DIM_COLOR;
          rendered.highlighted = neighbor;
          if (!neighbor) rendered.label = "";
        }
        return rendered;
      },
      edgeReducer: function (edge, data) {
        var rendered = Object.assign({}, data);
        if (selectedNode) {
          var endpoints = graph.extremities(edge);
          var touches =
            endpoints[0] === selectedNode || endpoints[1] === selectedNode;
          rendered.color = touches ? rendered.color : DIM_COLOR;
          rendered.hidden = !touches && graph.order > 30;
        }
        return rendered;
      },
    });

    // One shared tooltip for hovered nodes and edges: node label + kind, or
    // the edge's kind/confidence/provenance line. Purely presentational —
    // every value it shows came from the gated endpoint.
    var tooltip = document.createElement("div");
    tooltip.className = "graph-tooltip";
    tooltip.hidden = true;
    mount.appendChild(tooltip);

    function moveTooltip(event) {
      var bounds = mount.getBoundingClientRect();
      tooltip.style.left = event.x - bounds.left + 12 + "px";
      tooltip.style.top = event.y - bounds.top + 12 + "px";
    }

    renderer.on("enterNode", function (event) {
      var node = event.node;
      tooltip.textContent = graph.getNodeAttribute(node, "label");
      tooltip.hidden = false;
      moveTooltip(event);
    });
    renderer.on("enterEdge", function (event) {
      var text = graph.getEdgeAttribute(event.edge, "tooltip");
      if (!text) return;
      tooltip.textContent = text;
      tooltip.hidden = false;
      moveTooltip(event);
    });
    renderer.on("move", moveTooltip);
    renderer.on("leaveNode", hideTooltip);
    renderer.on("leaveEdge", hideTooltip);
    function hideTooltip() {
      tooltip.hidden = true;
    }

    // Click-to-focus: one node's neighborhood stays colored, the rest dims,
    // and the focus card offers the click-through when the viewer's tier has
    // a page for that kind. Clicking the background clears the focus.
    var card = document.createElement("aside");
    card.className = "graph-card";
    card.hidden = true;
    mount.appendChild(card);

    function clearFocus() {
      selectedNode = null;
      card.hidden = true;
      renderer.refresh();
    }

    renderer.on("clickNode", function (event) {
      var node = event.node;
      selectedNode = node;
      var kind = graph.getNodeAttribute(node, "kind");
      var template = hrefTemplate(mount, kind);
      var label = graph.getNodeAttribute(node, "label");
      // Built with DOM APIs, not innerHTML: the label is API data, and data
      // never reaches an HTML parser here.
      card.textContent = "";
      var title = document.createElement("strong");
      title.textContent = label;
      card.appendChild(title);
      if (kind) {
        card.appendChild(document.createElement("span")).textContent = kind;
      }
      if (template) {
        var link = document.createElement("a");
        link.className = "nav-link graph-card__link";
        link.href = template.replace("{id}", encodeURIComponent(node));
        link.textContent = "Open record \u2192";
        card.appendChild(link);
      }
      card.hidden = false;
      renderer.refresh();
    });
    renderer.on("clickStage", clearFocus);

    window.addEventListener("resize", function () {
      renderer.refresh();
    });

    setStatus(mount, "");
  }

  async function mountAll(mounts) {
    if (!mounts.length) return;
    var Sigma, Graph;
    try {
      await loadScript("/public/vendor/graphology/graphology.umd.min.js");
      await loadScript("/public/vendor/sigma/sigma.min.js");
      Graph = window.graphology;
      Sigma = window.Sigma;
      if (!Graph || !Sigma) throw new Error("graph libraries did not load");
    } catch (error) {
      mounts.forEach(function (mount) {
        setStatus(mount, "The graph renderer could not be loaded.", true);
      });
      return;
    }
    mounts.forEach(function (mount) {
      mountViewer(mount, Graph, Sigma);
    });
  }

  function enhance() {
    mountAll(Array.prototype.slice.call(document.querySelectorAll(".graph-view[data-endpoint]")));
  }

  if (typeof document !== "undefined") {
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", enhance);
    } else {
      enhance();
    }
  }

  // Headless smoke surface: the pure fetch→build→layout pipeline, no DOM.
  if (typeof module === "object" && module.exports) {
    module.exports = {
      NODE_COLORS: NODE_COLORS,
      nodeColor: nodeColor,
      edgeStyle: edgeStyle,
      edgeTooltip: edgeTooltip,
      buildGraph: buildGraph,
      layout: layout,
    };
  }
})();
