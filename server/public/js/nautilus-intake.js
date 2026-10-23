/* Progressive enhancement: the shared contact form remains usable without JS. */
(() => {
  const root = document.querySelector('[data-nautilus-intake]');
  if (!root || root.dataset.ready) return;
  const form = root.querySelector('form');
  const email = form.querySelector('[name="email"]');
  const phone = form.querySelector('[name="phone"]');
  const consent = form.querySelector('[name="sms_consent"]');
  const submit = form.querySelector('[type="submit"]');
  if (!email || !phone || !consent || !submit) return;
  const groups = [email.closest('.nav-field'), phone.closest('.nav-field'), consent.closest('.nav-field')];
  if (groups.some(group => !group)) return;
  root.dataset.ready = 'true';
  const titles = root.dataset.steps.split('|');
  const heading = document.createElement('h3');
  heading.tabIndex = -1;
  heading.setAttribute('aria-live', 'polite');
  const progress = document.createElement('progress');
  progress.max = 3;
  progress.setAttribute('aria-label', root.dataset.steps.replaceAll('|', ', '));
  const review = document.createElement('dl');
  const back = document.createElement('button');
  back.type = 'button';
  back.className = 'nav-btn';
  back.textContent = root.dataset.back;
  const next = document.createElement('button');
  next.type = 'button';
  next.className = 'nav-btn nav-btn--primary';
  next.textContent = root.dataset.next;
  submit.textContent = root.dataset.submit;
  form.prepend(heading, progress);
  submit.before(review, back, next);
  let step = 0;
  function render(focus) {
    heading.textContent = titles[step];
    progress.value = step + 1;
    groups.forEach((group, index) => { group.hidden = step === 2 || (step === 0 ? index === 2 : index !== 2); });
    back.hidden = step === 0;
    next.hidden = step === 2;
    submit.hidden = step !== 2;
    review.hidden = step !== 2;
    if (step === 2) {
      review.replaceChildren();
      [email, phone, consent].forEach(input => {
        const term = document.createElement('dt');
        term.textContent = form.querySelector(`label[for="${input.id}"]`)?.textContent || input.name;
        const value = document.createElement('dd');
        if (input.type === 'checkbox') {
          const choice = document.createElement('input');
          choice.type = 'checkbox';
          choice.checked = input.checked;
          choice.disabled = true;
          choice.setAttribute('aria-label', term.textContent);
          value.append(choice);
        } else {
          value.textContent = input.value;
        }
        review.append(term, value);
      });
      const disclosure = groups[2].querySelector('.nav-field__help')?.cloneNode(true);
      if (disclosure) {
        disclosure.removeAttribute('id');
        const notice = document.createElement('dd');
        notice.append(disclosure);
        review.append(notice);
      }
    }
    if (focus) heading.focus();
  }
  back.addEventListener('click', () => { step--; render(true); });
  next.addEventListener('click', () => {
    if (step === 0 && ![email, phone].every(input => input.reportValidity())) return;
    step++; render(true);
  });
  form.addEventListener('submit', event => {
    if (step !== 2) { event.preventDefault(); next.click(); }
  });
  form.addEventListener('invalid', () => { step = 0; render(false); }, true);
  render(false);
})();
