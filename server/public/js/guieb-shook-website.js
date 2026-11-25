"use strict";

// Header: see-through over the photo, solid cream with the navy wordmark once the page scrolls.
    (function () {
      var header = document.querySelector('.site-header');
      var toggle = header.querySelector('.menu-toggle');
      function setOpen(open) {
        header.classList.toggle('menu-open', open);
        toggle.setAttribute('aria-expanded', open);
        toggle.setAttribute('aria-label', open ? 'Close menu' : 'Open menu');
      }
      toggle.addEventListener('click', function () { setOpen(!header.classList.contains('menu-open')); });
      header.querySelectorAll('.nav a').forEach(function (a) { a.addEventListener('click', function () { setOpen(false); }); });
      header.querySelectorAll('.sub-toggle').forEach(function (b) {
        b.addEventListener('click', function () {
          var item = b.parentElement, open = !item.classList.contains('is-open');
          header.querySelectorAll('.has-menu.is-open').forEach(function (o) {
            if (o !== item) { o.classList.remove('is-open'); o.querySelector('.sub-toggle').setAttribute('aria-expanded', 'false'); }
          });
          item.classList.toggle('is-open', open);
          b.setAttribute('aria-expanded', open);
        });
      });
      if (!document.querySelector('.hero')) return;
      var ticking = false;
      function update() {
        ticking = false;
        header.classList.toggle('is-solid', header.getBoundingClientRect().top <= 0 && window.scrollY > 40);
      }
      window.addEventListener('scroll', function () {
        if (!ticking) { ticking = true; requestAnimationFrame(update); }
      }, { passive: true });
      update();
    })();

    if (document.querySelector('.hero')) {
// Hero slideshow: crossfades every 7 seconds; square markers choose a slide; pause button stops it.
    // Starts paused for visitors who set "reduce motion".
    (function () {
      var hero = document.querySelector('.hero');
      var slides = hero.querySelectorAll('.slide');
      var dots = hero.querySelectorAll('.dots button');
      var pause = hero.querySelector('.pause');
      var i = 0, timer = null;
      var paused = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
      // Background video: one player for all slides. It stays on its still frame for "reduce motion"
      // and pauses with the slideshow. A slide naming a different clip swaps the file.
      var media = hero.querySelector('.hero-media');
      var video = hero.querySelector('.hero-video');
      video.classList.add('is-shown');
      if (video && paused) { video.removeAttribute('autoplay'); video.pause(); }
      // One player per clip, created when first needed. Changing clips crossfades between players
      // in step with the text, and the new clip starts from its beginning, so the Williamsburg
      // Bridge (slide 1) always hands off to the Golden Gate (slide 2) at the matching moment.
      // Each clip comes in three sizes (built by tools/build-v4-video.sh). Pick by how many device
      // pixels the full-width hero covers: phones and small screens keep the light 1920 file, most
      // laptops get 2560, and large high-resolution screens (about 3000+ pixels wide) get 4K.
      var px = Math.max(window.innerWidth, 1) * (window.devicePixelRatio || 1);
      var size = px >= 3000 ? '-4k' : px > 2000 ? '-2560' : '';
      function clipSrc(name) { return '/assets/img/guieb-shook/video/' + name + size + '.mp4'; }
      var players = {};
      // The first clip is named in data-src, not src, so the browser downloads only the size picked here
      // (it used to fetch the 1920 file and then the 2560 one on laptops; 2026-11-25). The poster shows meanwhile.
      var firstClip = slides[0].dataset.video;
      if (!paused) video.src = clipSrc(firstClip);
      players[firstClip] = video;
      hero.setAttribute('data-clip', firstClip);
      function setVideo(name) {
        if (!video || !name) return;
        var next = players[name];
        if (next === video) return;
        if (!next) {
          next = document.createElement('video');
          next.className = 'hero-video';
          next.muted = true; next.loop = true; next.playsInline = true;
          next.setAttribute('muted', ''); next.setAttribute('playsinline', ''); next.setAttribute('aria-hidden', 'true');
          next.preload = 'auto';
          next.poster = '/assets/img/guieb-shook/video/' + name + '.jpg';
          if (!paused) next.src = clipSrc(name);
          media.insertBefore(next, media.firstChild);
          players[name] = next;
        }
        var prev = video;
        video = next;
        try { next.currentTime = 0; } catch (e) {}
        if (!paused) next.play().catch(function () {});
        next.classList.add('is-shown');
        prev.classList.remove('is-shown');
        hero.setAttribute('data-clip', name);  /* lets a clip have its own shading (see styles.css) */
        setTimeout(function () { if (prev !== video) prev.pause(); }, 1300);
      }
      function show(n) {
        slides[i].classList.remove('is-active');
        slides[i].setAttribute('aria-hidden', 'true');
        dots[i].classList.remove('is-active');
        dots[i].setAttribute('aria-pressed', 'false');
        i = (n + slides.length) % slides.length;
        slides[i].classList.add('is-active');
        slides[i].removeAttribute('aria-hidden');
        dots[i].classList.add('is-active');
        dots[i].setAttribute('aria-pressed', 'true');
        setVideo(slides[i].getAttribute('data-video'));
      }
      function start() { stop(); if (!paused) timer = setInterval(function () { show(i + 1); }, 7000); }
      function stop() { if (timer) clearInterval(timer); timer = null; }
      function setPaused(p) {
        paused = p;
        pause.classList.toggle('is-paused', p);
        pause.setAttribute('aria-label', p ? 'Play slideshow' : 'Pause slideshow');
        if (p) {
          video.pause();
        } else {
          if (!video.getAttribute('src')) video.src = clipSrc(slides[i].dataset.video);
          video.play().catch(function () {});
        }
        start();
      }
      dots.forEach(function (d, n) { d.addEventListener('click', function () { show(n); start(); }); });
      pause.addEventListener('click', function () { setPaused(!paused); });
      setPaused(paused);
    })();

    // About scroll story: as the pinned stage scrolls by, the two halves part to the sides (leveling as they go),
    // a gold seam draws where they met, then the heading and text rise into the gap one beat at a time.
    (function () {
      var track = document.getElementById('as-track') || document.getElementById('about');
      var comb = document.getElementById('as-comb'), law = document.getElementById('as-law'), tech = document.getElementById('as-tech');
      var seam = document.getElementById('as-seam');
      var items = Array.prototype.slice.call(document.querySelectorAll('#as-copy > *'));
      var rule = document.querySelector('#as-copy .as-rule');
      var phone = window.matchMedia('(max-width: 860px)'), still = window.matchMedia('(prefers-reduced-motion: reduce)');
      function clamp(x) { return Math.max(0, Math.min(1, x)); }
      function seg(p, a, b) { return clamp((p - a) / (b - a)); }
      function ease(t) { return t < .5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2; }
      var inner = track.querySelector('.as-inner'), copy = document.getElementById('as-copy');
      // Typed text (2026-11-25; on the site: "scroll" with the cursor, chosen on photo-lab/about-typing.html): with data-type="scroll", "timed" or "first" (see firstFrame) on #as-copy, the paragraphs appear
      // letter by letter, one after another, after the heading. "scroll" ties the letters to the scroll position (so it
      // reverses); "timed" types at a steady pace once the visitor reaches each paragraph. Each letter is its own span,
      // transparent until shown, so the layout never moves and screen readers read the whole text. data-caret="on"
      // adds a thin bronze cursor. Nothing types for "reduce motion": the text is simply there.
      var paras = items.filter(function (el) { return el.tagName === 'P'; }), tw = null;
      // preview links: index.html?type=first&caret=on shows a typing mode without changing the page
      function typeMode() { return still.matches ? '' : (copy.getAttribute('data-type') || ''); }
      function wrapText() {
        if (tw) return tw;
        var total = 0;
        tw = paras.map(function (p) {
          var chars = [];
          (function walk(n) { Array.prototype.slice.call(n.childNodes).forEach(function (c) {
            if (c.nodeType === 3) {
              var f = document.createDocumentFragment(), t = c.textContent;
              for (var i = 0; i < t.length; i++) { var sp = document.createElement('span'); sp.className = 'tw'; sp.textContent = t[i]; f.appendChild(sp); chars.push(sp); }
              c.parentNode.replaceChild(f, c);
            } else if (c.nodeType === 1) walk(c); }); })(p);
          var links = Array.prototype.slice.call(p.querySelectorAll('a')).map(function (a) {
            return { a: a, last: chars.indexOf(a.querySelectorAll('.tw')[a.querySelectorAll('.tw').length - 1]) }; });
          links.forEach(function (l) { l.a.classList.add('tw-pending'); });   // no underline before its letters appear
          var o = { p: p, chars: chars, n: chars.length, shown: 0, links: links, from: total, t0: 0 };
          total += chars.length; return o;
        });
        tw.total = total;
        return tw;
      }
      function showChars(o, k) {           // show the first k letters of a paragraph
        k = Math.max(0, Math.min(o.n, k));
        if (k === o.shown) return;
        var a = Math.min(k, o.shown), b = Math.max(k, o.shown), on = k > o.shown;
        for (var i = a; i < b; i++) o.chars[i].classList.toggle('on', on);
        o.links.forEach(function (l) { l.a.classList.toggle('tw-pending', k <= l.last); });
        o.shown = k;
      }
      function undim(o) { if (o.dim) { o.chars.forEach(function (c) { c.style.opacity = ''; }); o.dim = false; } }
      function unwrapAll() { if (tw) tw.forEach(function (o) { undim(o); showChars(o, o.n); }); placeCaret(); }
      // One blinking cursor, after the last letter shown, wherever the typing has got to (it also waits there, blinking,
      // between paragraphs); gone before the first letter and once the last paragraph is complete.
      var caretAt = null;
      function placeCaret() {
        var at = null;
        if (tw && typeMode() && copy.getAttribute('data-caret') === 'on') {
          var last = null; tw.forEach(function (o) { if (o.shown > 0) last = o; });
          if (last && !(last === tw[tw.length - 1] && last.shown >= last.n) &&
              !(typeMode() === 'first' && tw[0].done)) at = last.chars[last.shown - 1];
        }
        if (at === caretAt) return;
        if (caretAt) caretAt.classList.remove('tw-caret');
        if (at) at.classList.add('tw-caret');
        caretAt = at;
      }
      var timer = null;
      // Honeycomb pieces (2026-11-25): each hexagon of each half is drawn on its own small canvas from the half's video,
      // so the pieces can drift apart one by one as the text types. The video plays on, invisible, underneath; the
      // canvases copy its picture every frame (the poster until the video has a frame). If anything is missing, the
      // halves stay as they were, clipped by the SVG shapes.
      var cells = (function () {
        var list = [], ok = true, spread = '', ext = {};
        function polys(id) {
          var d = document.querySelector('#' + id + ' path').getAttribute('d');
          return d.split('Z').map(function (sp) { return (sp.match(/-?\d*\.?\d+/g) || []).map(Number); })
                  .filter(function (n) { return n.length >= 12; })
                  .map(function (n) { var pts = []; for (var i = 0; i < n.length; i += 2) pts.push([n[i], n[i + 1]]); return pts; });
        }
        try {
          [[law, 'about-hex-b'], [tech, 'about-hex-a']].forEach(function (pair) {
            var fig = pair[0], video = fig.querySelector('video'), poster = new Image();
            poster.src = video.getAttribute('poster');
            var wrap = document.createElement('div'); wrap.className = 'as-cells'; wrap.setAttribute('aria-hidden', 'true');
            var ps = polys(pair[1]);
            var cx = 0, cy = 0; ps.forEach(function (pt) { pt.forEach(function (q) { cx += q[0]; cy += q[1]; }); });
            var count = ps.reduce(function (a, pt) { return a + pt.length; }, 0); cx /= count; cy /= count;
            ps.forEach(function (pt) {
              var x0 = Math.min.apply(null, pt.map(function (q) { return q[0]; })), x1 = Math.max.apply(null, pt.map(function (q) { return q[0]; }));
              var y0 = Math.min.apply(null, pt.map(function (q) { return q[1]; })), y1 = Math.max.apply(null, pt.map(function (q) { return q[1]; }));
              var c = document.createElement('canvas'); c.className = 'as-cell';
              c.style.left = (x0 * 100) + '%'; c.style.top = (y0 * 100) + '%';
              c.style.width = ((x1 - x0) * 100) + '%'; c.style.height = ((y1 - y0) * 100) + '%';
              c.style.clipPath = 'polygon(' + pt.map(function (q) {
                return ((q[0] - x0) / (x1 - x0) * 100).toFixed(2) + '% ' + ((q[1] - y0) / (y1 - y0) * 100).toFixed(2) + '%'; }).join(',') + ')';
              wrap.appendChild(c);
              list.push({ c: c, fig: fig, video: video, poster: poster, box: [x0, y0, x1 - x0, y1 - y0],
                          dir: [(x0 + x1) / 2 - cx, (y0 + y1) / 2 - cy] });
            });
            // the pieces nearest the text hold still and the others spread away from it, so nothing drifts into the text:
            // shift the half's horizontal spread so its innermost piece stays put (law is on the left, tech on the right)
            var mine = list.filter(function (o) { return o.fig === fig; }), xs = mine.map(function (o) { return o.dir[0]; });
            var inward = fig === law ? Math.max.apply(null, xs) : Math.min.apply(null, xs);
            mine.forEach(function (o) { o.dir[0] -= inward; });
            ext[fig.id] = { a0: Math.min.apply(null, mine.map(function (o) { return o.box[0]; })),
                            a1: Math.max.apply(null, mine.map(function (o) { return o.box[0] + o.box[2]; })),
                            reach: Math.max.apply(null, mine.map(function (o) { return Math.abs(o.dir[0]); })) };
            fig.appendChild(wrap);
            fig.classList.add('has-cells');
          });
        } catch (e) { ok = false; }
        function draw() {
          list.forEach(function (o) {
            var fw = o.fig.offsetWidth, fh = o.fig.offsetHeight; if (!fw) return;
            var dpr = Math.min(2, window.devicePixelRatio || 1);
            var cw = Math.round(o.box[2] * fw * dpr), ch = Math.round(o.box[3] * fh * dpr);
            if (o.c.width !== cw || o.c.height !== ch) { o.c.width = cw; o.c.height = ch; }
            var src = o.video.readyState >= 2 ? o.video : (o.poster.complete && o.poster.naturalWidth ? o.poster : null);
            if (!src) return;
            var vw = src.videoWidth || src.naturalWidth, vh = src.videoHeight || src.naturalHeight;
            var k = Math.max(fw / vw, fh / vh), ox = (fw - vw * k) / 2, oy = (fh - vh * k) / 2;   // object-fit: cover
            var bx = o.box[0] * fw, by = o.box[1] * fh, bw = o.box[2] * fw, bh = o.box[3] * fh;
            o.c.getContext('2d').drawImage(src, (bx - ox) / k, (by - oy) / k, bw / k, bh / k, 0, 0, cw, ch);
          });
        }
        var running = false, near = false;
        function loop() { if (!near) { running = false; return; } draw(); requestAnimationFrame(loop); }
        if (ok) new IntersectionObserver(function (es) { near = es[0].isIntersecting;
          if (near && !running) { running = true; requestAnimationFrame(loop); } }, { rootMargin: '200px 0px' }).observe(track);
        return {
          ok: ok,
          // move each piece away from its half's centre: e from 0 (joined) to 1 (fully apart), by fx and fy of the half's
          // size (less sideways than up and down, so the outer pieces stay on screen)
          // fx may be a number, or one per half ({ 'as-law': .., 'as-tech': .. }) so both halves end the same width
          set: function (e, fx, fy) {
            var key = e + '|' + JSON.stringify(fx) + '|' + fy;
            if (!ok || key === spread) return; spread = key;
            list.forEach(function (o) { var fw = o.fig.offsetWidth, fh = o.fig.offsetHeight, f = typeof fx === 'number' ? fx : fx[o.fig.id];
              o.c.style.transform = e ? 'translate(' + (o.dir[0] * fw * f * e).toFixed(1) + 'px,' + (o.dir[1] * fh * fy * e).toFixed(1) + 'px)' : ''; });
          },
          ext: function (fig) { return ok ? ext[fig.id] : null; },
          draw: draw
        };
      })();
      // The closed honeycomb, centred by its hexagons rather than its box (the box has more empty space on the left)
      function centerComb() {
        var eL = cells.ext(law), eT = cells.ext(tech); if (!eL || !eT) return;
        var lo = Math.min(eL.a0 * .693, .307 + eT.a0 * .693), hi = Math.max(eL.a1 * .693, .307 + eT.a1 * .693);
        comb.style.marginLeft = ((.5 - (lo + hi) / 2) * comb.offsetWidth).toFixed(1) + 'px';
      }
      // windows: where on the scroll the paragraphs type (they share it in proportion to their length), and where the
      // button then rises; desktop uses the share of the pinned scroll, phones multiples of the screen height
      // "first": only the opening sentence types, at a slow steady pace (18 letters a second) once the visitor reaches
      // it, however fast they scroll, so the effect is seen. Everything else keeps following the scroll exactly as before
      // (nothing waits for the typing): the rest of that paragraph fades in on the paragraph's own scroll beat (t),
      // once its sentence has finished typing.
      // While the sentence types, the page holds at the point where it began: scrolling further is paused (wheel,
      // touch and keys), so nothing below it can be reached until it has finished; then the story carries on as before.
      var holdY = 0;
      function holding() { return typeMode() === 'first' && tw && tw[0].reached && !tw[0].done; }
      function hold(e) {
        if (!holding()) return;
        if (e.type === 'keydown' && ['ArrowDown', 'PageDown', 'End', ' ', 'Spacebar'].indexOf(e.key) < 0) return;
        if (e.type === 'wheel' && e.deltaY <= 0) return;
        e.preventDefault();
      }
      addEventListener('wheel', hold, { passive: false });
      addEventListener('touchmove', hold, { passive: false });
      addEventListener('keydown', hold);
      function firstFrame(pos, a, t, lockY) {
        var T = wrapText(), o = T[0];
        for (var j = 1; j < T.length; j++) showChars(T[j], T[j].n);
        if (o.k1 === undefined) {
          var txt = o.chars.map(function (c) { return c.textContent; }).join(''), e = txt.indexOf('. ');
          o.k1 = e < 0 ? o.n : e + 1;
        }
        o.p.style.opacity = 1; o.p.style.transform = 'none';
        o.reached = pos >= a; holdY = lockY;
        if (o.reached && !o.done && scrollY > lockY + 2) scrollTo({ top: lockY, behavior: 'instant' });   // a fast fling lands here
        if (!o.reached) { o.t0 = 0; o.done = false; showChars(o, 0); }
        else if (!o.done && !timer) timer = requestAnimationFrame(tickFirst);
        placeCaret();
        var r = o.done ? t : 0;                       // the rest of the paragraph, on the scroll
        for (var i = o.k1; i < o.n; i++) o.chars[i].style.opacity = r;
        o.dim = true;
      }
      function tickFirst(now) {
        timer = null; if (typeMode() !== 'first') return;
        var o = tw[0]; if (!o.reached || o.done) return;
        if (!o.t0) o.t0 = now;
        var k = Math.floor((now - o.t0) * 0.018);
        if (k < o.k1) { showChars(o, k); placeCaret(); timer = requestAnimationFrame(tickFirst); return; }
        for (var i = o.k1; i < o.n; i++) o.chars[i].style.opacity = 0;
        showChars(o, o.n); o.done = true; placeCaret(); frame();
      }
      function typeFrame(pos, a, b, btnBeat) {
        var mode = typeMode(), T = wrapText();
        T.forEach(undim);
        T.forEach(function (o) {
          var st = a + (b - a) * o.from / T.total, en = a + (b - a) * (o.from + o.n) / T.total;
          o.p.style.opacity = 1; o.p.style.transform = 'none';
          if (mode === 'scroll') { showChars(o, Math.round(seg(pos, st, en) * o.n)); }
          else {                              // timed: start when reached, after the paragraph before has finished
            o.reached = pos >= st;
            if (!o.reached) { o.t0 = 0; showChars(o, 0); }
          }
        });
        placeCaret();
        if (mode === 'timed' && !timer) timer = requestAnimationFrame(tick);
        var done = T[T.length - 1].shown >= T[T.length - 1].n;
        return mode === 'timed' && !done ? 0 : seg(pos, btnBeat[0], btnBeat[1]);   // the button comes after the last letter
      }
      function tick(now) {
        timer = null; if (typeMode() !== 'timed') return;
        var busy = false, prevDone = true;
        tw.forEach(function (o) {
          if (o.reached && prevDone) {
            if (!o.t0) o.t0 = now - o.shown / 0.075;               // 75 letters a second, picking up where it was
            showChars(o, Math.floor((now - o.t0) * 0.075));
          }
          if (o.shown < o.n && o.reached && prevDone) busy = true;
          prevDone = prevDone && o.shown >= o.n;
        });
        placeCaret();
        if (busy) timer = requestAnimationFrame(tick); else frame();   // let the button rise once the typing is done
      }
      // Phones: same story, re-staged for a tall, narrow screen. The halves part and drift up into the top
      // corners, the heading and text rise beneath them, then the whole stage glides up so the rest of the
      // text can be read before the page moves on. Everything is tied to scroll position, so it reverses.
      function phoneFrame() {
        if (still.matches) {
          track.style.height = ''; inner.style.transform = ''; track.style.scrollMarginTop = '';
          seam.style.top = seam.style.height = '';
          [law, tech].concat(items).forEach(function (e) { e.style.transform = ''; e.style.opacity = ''; }); unwrapAll(); cells.set(0, 0, 0);
          comb.style.marginLeft = ''; return;
        }
        var H = innerHeight, W = innerWidth;
        var over = Math.max(0, .37 * H + copy.offsetHeight + 28 - H);   // text that won't fit below the halves
        var A = 1.25 * H, total = A + over + .2 * H;                      // pinned scroll distance
        track.style.height = (total + H) + 'px';
        track.style.scrollMarginTop = (-A) + 'px';   // the menu's About link lands with the text in place
        var y = Math.max(0, Math.min(total, -track.getBoundingClientRect().top));
        var s = ease(seg(y, .05 * H, .6 * H));
        centerComb();
        var cw = comb.offsetWidth, chh = cw / (115.84 / 104.92), left = comb.getBoundingClientRect().left, top = .35 * H - chh / 2;
        var lawX = left + .3465 * cw, lawY = top + (.2349 + .38255) * chh, techX = left + (.307 + .3465) * cw, techY = top + .38255 * chh;
        var ty = .235 * H, k = 1 - .5 * s, e = ease(seg(y, .6 * H, 1.25 * H));
        var eL = cells.ext(law), eT = cells.ext(tech), lawTX = .27 * W, techTX = .73 * W, fxs = .3;
        if (eL && eT) {
          // centred on the page, as on desktop: equal outer margins at every stage, and both halves the same width once
          // their pieces spread; the gap between the halves never drops below G
          var fw = law.offsetWidth, G0 = .07 * W;
          var wL0 = (eL.a1 - eL.a0) * fw * k, wT0 = (eT.a1 - eT.a0) * fw * k;
          var D = W / 2 - G0 - .1 * W;
          var fxL = clamp((D - wL0) / (eL.reach * fw * k)), fxT = clamp((D - wT0) / (eT.reach * fw * k));
          var wL = wL0 + eL.reach * fw * k * fxL * e, wT = wT0 + eT.reach * fw * k * fxT * e;
          var G = G0 + Math.abs(wT - wL) / 2, M = (W - wL - wT - 2 * G) / 2;
          lawTX = (M + wL) - (eL.a1 - .5) * fw * k;           // the half's centre, from where its inner edge must sit
          techTX = (W - M - wT) - (eT.a0 - .5) * fw * k;
          fxs = { 'as-law': fxL, 'as-tech': fxT };
        }
        law.style.transform = 'translate(' + ((lawTX - lawX) * s) + 'px,' + ((ty - lawY) * s) + 'px) scale(' + k + ')';
        tech.style.transform = 'translate(' + ((techTX - techX) * s) + 'px,' + ((ty - techY) * s) + 'px) scale(' + k + ')';
        // the gold seam draws down between them as they part, then fades as the text arrives (as on desktop)
        var st = ty - .23 * W, sb = ty + .23 * W;   // from just above the parted halves to just below them
        seam.style.top = st + 'px'; seam.style.height = (sb - st) + 'px';
        seam.style.transform = 'translateX(-50%) scaleY(' + ease(seg(y, .15 * H, .55 * H)) + ')';
        seam.style.opacity = .7 * (1 - seg(y, .62 * H, .9 * H));
        cells.set(e, fxs, .45);
        var beats = [[.42, .66], [.5, .74], [.58, .82], [.68, .92], [.78, 1.02], [.88, 1.12], [.98, 1.22]];
        var typing = typeMode(), first = typing === 'first';
        if (first) firstFrame(y, .58 * H, ease(seg(y, .58 * H, .82 * H)), track.getBoundingClientRect().top + scrollY + .58 * H);
        var bt = typing && !first ? ease(typeFrame(y, .6 * H, 1.2 * H, [1.18 * H, 1.32 * H])) : 0;
        if (!typing) unwrapAll();
        items.forEach(function (el, i) { var b = beats[Math.min(i, beats.length - 1)], t = ease(seg(y, b[0] * H, b[1] * H));
          if (first && el === paras[0]) return;
          else if (typing && !first && el.tagName === 'P') return;
          if (typing && !first && el.tagName === 'A') t = bt;
          el.style.opacity = el === rule ? 1 : t; el.style.transform = el === rule ? 'scaleX(' + t + ')' : 'translateY(' + (18 * (1 - t)) + 'px)'; });
        inner.style.transform = 'translateY(' + (-Math.max(0, Math.min(over, y - A))) + 'px)';
      }
      function frame() {
        if (holding() && scrollY > holdY + 2) scrollTo({ top: holdY, behavior: 'instant' });   // hold before drawing
        if (phone.matches) { phoneFrame(); return; }
        track.style.height = ''; inner.style.transform = ''; track.style.scrollMarginTop = '';
        seam.style.top = seam.style.height = '';
        var span = Math.max(1, track.offsetHeight - innerHeight);
        var p = still.matches ? 1 : clamp(-track.getBoundingClientRect().top / span);
        var s = ease(seg(p, .06, .46));
        centerComb();
        var cw = comb.offsetWidth, cx = comb.getBoundingClientRect().left, k = 1 - .18 * s, W = innerWidth;
        var dy = cw / (115.84 / 104.92) * .2349 / 2 * s;
        var eL = cells.ext(law), eT = cells.ext(tech), e = ease(seg(p, .48, .9));
        if (eL && eT) {
          // Centred on the page at every stage (2026-11-25): the two halves always have equal outer margins. They are
          // different widths (four hexagons and five), so their gaps to the text differ a little until the pieces spread;
          // the narrower gap never drops below G (clear of the text). Fully spread, both halves are the same width, so
          // margins and gaps all match.
          var fw = law.offsetWidth, G0 = copy.offsetWidth / 2 + .04 * W;
          var wL0 = (eL.a1 - eL.a0) * fw * k, wT0 = (eT.a1 - eT.a0) * fw * k;
          var D = W / 2 - G0 - Math.max(.03 * W, 20);                        // the width each half ends at
          var fxL = clamp((D - wL0) / (eL.reach * fw * k)), fxT = clamp((D - wT0) / (eT.reach * fw * k));
          var wL = wL0 + eL.reach * fw * k * fxL * e, wT = wT0 + eT.reach * fw * k * fxT * e;   // widths now
          var G = G0 + Math.abs(wT - wL) / 2, M = (W - wL - wT - 2 * G) / 2;   // the gap to the text, and the margin
          var dL = (M + wL) - (cx + fw / 2 + (eL.a1 - .5) * fw * k);          // law's inner edge (its inner pieces hold still)
          var dT = (W - M - wT) - (cx + .307 * cw + fw / 2 + (eT.a0 - .5) * fw * k);
          law.style.transform = 'translateX(' + (dL * s) + 'px) translateY(' + (-dy) + 'px) scale(' + k + ')';
          tech.style.transform = 'translateX(' + (dT * s) + 'px) translateY(' + dy + 'px) scale(' + k + ')';
          cells.set(e, { 'as-law': fxL, 'as-tech': fxT }, .55);   // the pieces drift apart as the text types
        } else {
          var lawC = cx + .3465 * cw, techC = cx + (.307 + .3465) * cw, lawT = W * .16, techT = W * .84;
          law.style.transform = 'translateX(' + ((lawT - lawC) * s) + 'px) translateY(' + (-dy) + 'px) scale(' + k + ')';
          tech.style.transform = 'translateX(' + ((techT - techC) * s) + 'px) translateY(' + dy + 'px) scale(' + k + ')';
        }
        seam.style.transform = 'translateX(-50%) scaleY(' + ease(seg(p, .14, .34)) + ')'; seam.style.opacity = .7 * (1 - seg(p, .40, .52));
        var beats = [[.40, .52], [.46, .56], [.52, .62], [.58, .68], [.64, .74], [.70, .80], [.77, .87]];
        var typing = typeMode(), first = typing === 'first';
        if (first) firstFrame(p, .52, ease(seg(p, .52, .62)), track.getBoundingClientRect().top + scrollY + .52 * span);
        var bt = typing && !first ? ease(typeFrame(p, .50, .84, [.84, .92])) : 0;
        if (!typing) unwrapAll();
        items.forEach(function (el, i) { var b = beats[Math.min(i, beats.length - 1)], t = ease(seg(p, b[0], b[1]));
          if (first && el === paras[0]) return;
          else if (typing && !first && el.tagName === 'P') return;
          if (typing && !first && el.tagName === 'A') t = bt;
          el.style.opacity = el === rule ? 1 : t; el.style.transform = el === rule ? 'scaleX(' + t + ')' : 'translateY(' + (18 * (1 - t)) + 'px)'; });
      }
      var ticking = false;
      addEventListener('scroll', function () { if (!ticking) { ticking = true; requestAnimationFrame(function () { ticking = false; frame(); }); } }, { passive: true });
      addEventListener('resize', frame); frame();
      new IntersectionObserver(function (es) { es.forEach(function (e) { if (e.isIntersecting) comb.classList.add('in'); }); }, { threshold: .35 }).observe(comb);
    })();

    // Closing band: as the footer comes into view, the two bridges glide in toward the badge as the visitor scrolls.
    // Wide screens: they start at the edges of the content and meet the badge at the very bottom of the page.
    // Phones: the band slides under the sticky menu before the bottom, so there they start fully off screen as the
    // band comes into view and meet the badge when the band is about a third of the way down the screen, well clear of the menu.
    // The slogan stays still, as on every other page. Nothing moves for "reduce motion".
    (function () {
      var p = document.querySelector('.footer-announce'), band = document.querySelector('.closing-band');
      var header = document.querySelector('.site-header'), phone = window.matchMedia('(max-width: 860px)');
      if (!p || !band || window.matchMedia('(prefers-reduced-motion: reduce)').matches) return;
      var bk = band.querySelector('.bridge-brooklyn'), bay = band.querySelector('.bridge-bay'); if (!bk || !bay) return;
      var m = null, queued = false;
      function measure() {
        bk.style.transform = bay.style.transform = 'none';
        var br = bk.getBoundingClientRect(), yr = bay.getBoundingClientRect(), b = band.getBoundingClientRect();
        if (phone.matches) {
          m = { bx: -br.right - 4, yx: b.right - yr.left + 4 };   // just past each side of the screen
        } else {
          var pad = parseFloat(getComputedStyle(band).paddingLeft);
          var max = parseFloat(getComputedStyle(document.documentElement).getPropertyValue('--max')) || 1200;
          var inner = Math.max(pad, (b.width - max) / 2 + pad);
          // --bridge-start: 0 starts them at the edges of the content, 1 just off screen, in between partway out
          var f = parseFloat(getComputedStyle(band).getPropertyValue('--bridge-start')) || 0;
          var cx = b.left + inner - br.left, cy = b.right - inner - yr.right;
          m = { bx: cx + (-br.right - 4 - cx) * f, yx: cy + (b.right - yr.left + 4 - cy) * f };
        }
        draw();
      }
      function draw() {
        queued = false; if (!m) return;
        // positions are read each time, since images and videos above can change the page's height
        var vh = window.innerHeight, y = window.pageYOffset, begin, end;
        if (phone.matches) {
          var bandTop = band.getBoundingClientRect().top + y, menu = header ? header.getBoundingClientRect().bottom : 0;
          begin = bandTop - vh; end = bandTop - Math.max(menu, vh * 0.35);   // meet with the band about a third of the way down the screen
        } else {
          var top = p.getBoundingClientRect().top + y;
          begin = top - vh; end = Math.min(document.documentElement.scrollHeight - vh, top - vh * 0.15);
        }
        var k = 1 - Math.min(1, Math.max(0, (y - begin) / Math.max(1, end - begin)));
        bk.style.transform = 'translateX(' + (m.bx * k).toFixed(1) + 'px)';
        bay.style.transform = 'translateX(' + (m.yx * k).toFixed(1) + 'px)';
      }
      window.addEventListener('scroll', function () { if (!queued) { queued = true; requestAnimationFrame(draw); } }, { passive: true });
      window.addEventListener('resize', measure);
      window.addEventListener('load', measure);
      measure();
    })();
    }

    const reducedMotion = matchMedia('(prefers-reduced-motion: reduce)');
    const ambientVideos = document.querySelectorAll('.as-half video, .contact-video, .banner-video, .namesake-video');
    function syncPlayback(video) {
      if (reducedMotion.matches || video.dataset.inView !== 'true') {
        video.pause();
      } else {
        if (!video.getAttribute('src')) video.src = video.dataset.src;
        video.play().catch(() => {});
      }
    }
    const inView = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        entry.target.dataset.inView = String(entry.isIntersecting);
        syncPlayback(entry.target);
      }
    }, { rootMargin: '400px 0px' });
    for (const video of ambientVideos) inView.observe(video);
    reducedMotion.addEventListener('change', () => ambientVideos.forEach(syncPlayback));
