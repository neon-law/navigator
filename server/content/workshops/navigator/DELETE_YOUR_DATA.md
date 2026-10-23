---
kind: workshop
title: Delete Your Data
description: Privacy through business separation, estate planning, deletion rights, and everyday data hygiene.
---

# Delete Your Data

![DeleteYourData trash-bin logo](/public/brand/delete-your-data/logo.svg)

A 20-minute Portland BSides follow-up centered on autonomy. 60 content slides average about 20 seconds each, followed by
a CTF key slide. Keep the three-slide opening to about 90 seconds, use the factory section to explain the working
approach, and move quickly through transition slides. Existing illustrations recur where adjacent slides develop one
idea. The opening slide also carries the original recording link. Presenter notes and sources stay below the divider.
Factory claims checked against the repository October 23, 2026; AI links reviewed October 22, 2026. Legal sections
retain their October 21, 2026 review and need a current delivery check.

- I. The original talk
- II. Our autonomous software factory
- III. Local and cloud AI
- IV. Business separation and estate planning
- V. Death and digital control
- VI. State removal rights
- VII. Data hygiene
- VIII. Delete Your Data: what we cover

## I. The original talk

### Delete Your Data in 2022

![Delete Your Data trash-bin logo](/public/brand/delete-your-data/logo.svg)

[Watch the original 2022 talk](https://www.youtube.com/watch?v=rkzdIDiiaJg)

---

At BSides Portland in October 2022, Jacob Smith presented Delete Your Data's work helping people find personal
information online and request its deletion. The opening process was deliberately simple: identify the information,
identify its holder, and ask for removal. The talk then explained the legal framework that gives those requests force.

The nonprofit's approach combined discovery with action. People could identify particular accounts or use a service to
find information, then pursue deletion requests. The lesson for this audience is practical: knowing that information
exists is the beginning; knowing which request to send and where to send it makes the next step possible.

This follow-up carries that work forward into prevention and continuity. We look at what gets shared before signup,
which agreements govern business tools, and who can act after us. Keep this recap to about 45 seconds; the recording
link beneath the illustration lets people explore the original talk afterward.

Sources:

- [original recording][original]
- [transcript and talk listing][transcript]

[original]:
  <https://www.youtube.com/watch?v=rkzdIDiiaJg>
[transcript]:
  <https://allbsides.com/talk/rkzdIDiiaJg.html>

### 200 users, then we shut down

![A business connects a person to a service](img/delete-your-data/03-business.png)

---

We had great early growth: 200 users. But the processes took too long, and we had to shut down the project.

There was demand. The time required to carry out the work made it unsustainable. That experience gives us a practical
question for the next version: how do we make the process manageable?

### But now we have agents

![A model request sits within an organization boundary](img/delete-your-data/08-vertex.png)

---

But now we have agents, and self-replicating software factories.

The opportunity is to reduce repetitive work while keeping people in control of what information is shared and which
requests are made. That is the bridge from the original project to the tools and privacy choices in this talk.

## II. Our autonomous software factory

### From big tech engineering to law

![An engineer walks from a glass office toward a neighborhood law office](img/delete-your-data/art-01.png)

---

I left my job as a software engineer in big tech to become a lawyer. I am grateful we can start this project again with
the resources and time it needs.

### Teaching in the age of agents

![A coding class with mechanical helpers](img/delete-your-data/teaching-agents-ghibli.png)

---

My last job in tech was teaching others how to code. These days, everyone is vibing.

### Our autonomous software factory

![A software workshop with mechanical helpers](img/delete-your-data/software-factory-ghibli.png)

---

In my day-to-day work, I solve legal projects, each with its own repository. I file generic engineering issues in Linear
to improve the system. Client records and work product stay in approved systems. Public code changes contain reusable
improvements, with no client content.

Once issues are in Linear, agents use the `implement-issue` skill to work through them.

### Neon Law Navigator

![Neon Law logo](/public/logo.svg)

---

Neon Law Navigator is our software system for organizing legal work. It powers Delete Your Data and our collection of
firms.

Sources:

- [Navigator source code](https://github.com/neon-law/navigator)

### Security is continuing maintenance

![An eraser reduces an unnecessary trail](img/delete-your-data/19-hygiene.png)

---

Open sourcing brings security benefits and responsibilities. We choose deliberately boring technology, update our
dependencies, and review changes. We use providers such as Google and Cloudflare for parts of authentication and network
security. Configuration and application security remain our responsibility.

Sources:

- [Dependency update configuration](https://github.com/neon-law/navigator/blob/main/.github/dependabot.yml)

### Kubernetes here and in the cloud

![Matching computing gardens in a home and on a cloud island](img/delete-your-data/art-02.png)

---

The vulnerabilities discussed here at BSides remind us to maintain every layer of the stack. We put as much of the
system as practical into Kubernetes. A common workload description helps us run locally and in the cloud, with less
dependence on one provider's deployment model. The underlying hosts still need security updates.

Sources:

- [Kubernetes overview](https://kubernetes.io/docs/concepts/overview/)

### Short-lived work, deliberate cleanup

![Official Kubernetes Job resource icon](img/delete-your-data/kubernetes-job.png)

---

The direction we want for bounded inference work is a Kubernetes Job. Let each task finish, then clean up its resources.
Configure time limits and automatic cleanup explicitly; completed Jobs are not always removed by default.

We want short-lived credentials and scoped network access for each task. Tools such as Envoy can help enforce that
boundary when configured for it. This adds operational overhead, but gives us more control over what an agent can access
than an unrestricted process on a personal machine.

Sources:

- [Kubernetes Jobs](https://kubernetes.io/docs/concepts/workloads/controllers/job/)
- Icon: [Kubernetes community icons](https://github.com/kubernetes/community/tree/main/icons), CC BY 4.0.

### Rust makes memory rules explicit

![A balance represents an engineering tradeoff](img/delete-your-data/11-limits.png)

---

We use Rust. In my experience, writing it can take longer up front because the compiler makes us work through ownership
and borrowing. That effort helps prevent many memory errors before the program runs.

Sources:

- [Rust ownership](https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html)

### Share the code, invite scrutiny

![Records fan out for others to inspect](img/delete-your-data/04-registry.png)

---

We share our codebase so others can vet our security posture, inspect the privacy boundaries, and help us understand
costs. Public code lets someone ask a concrete question about an actual implementation.

Inspection is an opportunity to build justified trust. It does not certify the running deployment. Configuration,
permissions, provider agreements, and operational practice need to match the code's intended protections.

Sources:

- [Navigator source code](https://github.com/neon-law/navigator)

### Keep content out of telemetry

![Anonymous counts stay separate from sealed private letters](img/delete-your-data/art-03.png)

---

Our logging rule is identifiers and counts, never client content. We want enough operational evidence to diagnose a
failure and measure costs without putting prompts, document bodies, or personal details into telemetry.

Our system is multi-tenant: it serves multiple law firms and legal aid organizations.

## III. Local and cloud AI

### Choose the AI service agreement

![An organization selects the boundary around a model request](img/delete-your-data/07-fabric.png)

---

Choose a service agreement with data controls that fit your needs. Check the exact product's retention, training,
administrator access, and disclosure terms. Paying for a subscription alone does not establish those protections.

For example, Microsoft documents data-use and processing controls for Copilot in Fabric. Review the feature and tenant
settings you actually use.

Sources:

- [Fabric privacy and security][fabric]
- [notebooks and data-agent history][fabric-history]

[fabric]:
  <https://learn.microsoft.com/en-us/fabric/fundamentals/copilot-privacy-security>
[fabric-history]:
  <https://learn.microsoft.com/en-us/fabric/fundamentals/copilot-data-science-privacy-security>

### Local and cloud models

![A home computer connects to a distant cloud observatory](img/delete-your-data/art-04.png)

---

We share our setup to make it easier to swap models. Treat the model as a replaceable tool, like a database. Keep the
application's requirements and data boundaries explicit when changing providers.

Sources:

- [Google Cloud service terms](https://cloud.google.com/terms/service-terms)
- [Google retention guidance](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/vertex-ai-zero-data-retention)

### One application, two environments

![Matching workshops stand in a mountain cabin and on a floating island](img/delete-your-data/art-05.png)

---

Kubernetes provides a common way to describe workloads across a local machine and a cloud cluster. Keep configuration
for each environment explicit. Hardware, networking, secrets, and storage still need their own setup; portability needs
testing.

Sources:

- [Kubernetes overview](https://kubernetes.io/docs/concepts/overview/)

### Small models close to home

![A small computing machine sits among houseplants at home](img/delete-your-data/art-06.png)

---

Suitable hardware at home can serve a smaller model. Choose hardware with enough memory for the model and workload. Keep
the application interfaces consistent across local and cloud environments, and configure access to the database and
model endpoints explicitly. A shared Kubernetes setup still needs network and access controls.

Sources:

- [Kubernetes overview](https://kubernetes.io/docs/concepts/overview/)

### Frontier models by exception

![A craftsperson sends a difficult puzzle to a distant observatory](img/delete-your-data/art-07.png)

---

Use a frontier model when its capabilities justify sending the task to a cloud provider. Limit the information in the
request to what the task needs, and check the provider's terms. Vertex AI is one route to hosted models.

Sources:

- [Claude on Vertex AI](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/partner-models/claude)

### Give every token a budget

![A person budgets glowing coins beside a mechanical assistant](img/delete-your-data/art-08.png)

---

My recommendation is to think in cost per task and tokens consumed. Metered use makes each call visible and encourages
restraint; it does not enforce a cap by itself. Set budgets and application limits, and decide which tasks justify a
model call. Local inference also consumes hardware, electricity, and time.

### Cache repeated context carefully

![An archivist reuses reference cards while private letters stay sealed](img/delete-your-data/art-09.png)

---

Prompt caching can reuse supported repeated context and reduce repeated processing costs. Reusing successful removal
request patterns is a related idea for Delete Your Data. Model prompt caching has its own retention, pricing, and
provider controls.

Sources:

- [Claude prompt caching][claude-cache]

[claude-cache]:
  <https://docs.cloud.google.com/vertex-ai/generative-ai/docs/partner-models/claude/prompt-caching>

### A good old data dictionary

![An illustrated dictionary rests on a library lectern](img/delete-your-data/art-10.png)

---

Define your fields, meanings, permitted values, and sources in a data dictionary. Our
[glossary](https://www.neonlaw.com/glossary) gives the system a shared vocabulary.

### Always defer to code

![A clockwork calculator handles a task beside a resting mechanical helper](img/delete-your-data/art-11.png)

---

Agents help us create code. Durable workflows carry out repeatable steps, while ordinary code handles validation,
calculations, and stable rules.

## IV. Business separation and estate planning

### Before the next signup consider an entity

![A person chooses an account arrangement before sharing information](img/delete-your-data/02-signup.png)

---

Notice collection. Ask who benefits. Decide what to share. Add three tools: separate contact details, choose the service
agreement, and name a successor. These are separate ways to exercise control.

### Autonomy before and after

![A traveler considers a carefully packed case at a registration kiosk](img/delete-your-data/art-12.png)

---

The through-line is autonomy: choose what enters a service, choose the terms, and prepare someone to act later. Deletion
is one tool in that larger set. Today we move from prevention to succession to removal.

### A business-facing identity

![A shop faces the street while a garden protects a private home](img/delete-your-data/art-13.png)

---

For genuine business activity, use the LLC's name and appropriate business contact details. Keep the home address and
personal phone out of routine public-facing fields where permitted. The provider can still require an individual's
identity. An LLC is a contracting party, not an anonymous identity.

Sources:

- [Oregon business privacy alternatives][registry]

[registry]:
  <https://sos.oregon.gov/business/Pages/business-privacy.aspx>

### The registry can expose you

![A registry cabinet exposes a house-shaped card to the street](img/delete-your-data/art-14.png)

---

Check what the filing publishes before submitting it. Oregon requires physical addresses for specified fields. A
registered agent does not replace every address requirement. Choose lawful alternatives before creating new exposure.

Sources:

- [Oregon business privacy alternatives][registry]

[registry]:
  <https://sos.oregon.gov/business/Pages/business-privacy.aspx>

### Separate the channels

![Business contact channels separate a person from several services](img/delete-your-data/05-channels.png)

---

Illustrative setup: business email, business phone, appropriate business address, and an eligible business account. Keep
personal recovery details out of public profiles. The benefit is less casual linkage, not guaranteed anonymity.

### Ownership and access

![Two adults review a shop ledger and keys](img/delete-your-data/art-15.png)

---

Write down who owns each business account and who administers it. A business name on a profile does not tell you who can
recover it, export it, or close it. Review those permissions alongside the contact information.

### Use social media as your business

![A business controls a public page while a person manages access](img/delete-your-data/06-meta.png)

---

Example: publish the LLC's business name and contact channels on its Page; manage business assets through a business
portfolio. Keep personal details off the public Page where the platform permits. Meta still identifies its managers.
Contact information is visible to people added to the portfolio. Page Transparency can disclose management countries and
confirmed business owners. Public separation does not mean anonymity from Meta or coworkers.

Sources:

- [Meta portfolio setup][meta-setup]
- [Meta Page Transparency][meta-transparency]

[meta-setup]:
  <https://www.facebook.com/business/help/1710077379203657>
[meta-transparency]:
  <https://www.facebook.com/help/323314944866264>

### Estate planning follows ownership

![Trust documents connect an asset to its successor](img/delete-your-data/10-trust.png)

---

After separating the business, plan its continuity. Identify who succeeds to the LLC interest and who can administer its
accounts; coordinate the governing documents and provider access. A trustee may hold appropriate assets through a living
trust. An estate plan is not itself a signup identity. A decedent's estate is administered after death. A funded living
trust can avoid probate for trust assets; litigation and recorded transactions can still expose facts.

Sources:

- [California court explanation of living trusts][trust]
- [California digital-asset directions][directions]

[trust]:
  <https://www.occourts.org/self-help/self-help-probate/medical-financial-end-life/living-trusts>
[directions]:
  <https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=PROB&sectionNum=873.>

### A successor needs a route

![An elder shows a trusted adult a route through an archive garden](img/delete-your-data/art-16.png)

---

Ownership and practical access need to work together. Keep a private inventory of business accounts and the provider
process for changing their administrator. Coordinate that inventory with the estate plan and governing documents.

### Privacy expectations in court

![A balance represents privacy interests and access authority](img/delete-your-data/09-cases.png)

---

*Warshak*: hosted email contents can carry a reasonable expectation of privacy against government access. The Sixth
Circuit found a warrantless-access violation but applied the good-faith exception to suppression.

*Ziegler*: a private workplace office carried a privacy interest, but the employer validly consented to the search of
the office and business computer. The Ninth Circuit affirmed denial of suppression.

Neither case establishes that B2B accounts receive stronger privacy than personal accounts. Distinguish constitutional
search protection, contractual limits on provider use, and public visibility. Our practical inference: inspect access
rights and terms rather than treating the business label as a privacy guarantee.

Sources:

- [United States v. Warshak, 631 F.3d 266, 288 (6th Cir. 2010)][warshak]
- [United States v. Ziegler, 474 F.3d 1184, 1192–93 (9th Cir. 2007)][ziegler]

[warshak]:
  <https://app.midpage.ai/document/united-states-v-warshak-181032?utm_source=mcp&lines=328-331>
[ziegler]:
  <https://app.midpage.ai/document/united-states-v-ziegler-3040566?utm_source=mcp&lines=163-164>

### Three different privacy questions

![Three doors lead to a courtroom, archive, and advice room](img/delete-your-data/art-17.png)

---

Ask what strangers can see, what the provider can do, and what an authorized colleague can access. These are different
boundaries. Choose contact details, permissions, and contracts to address the boundary you care about.

### Separation has limits

![A garden fence has an open gate and gaps](img/delete-your-data/art-18.png)

---

Ask three questions: Who owns the account? What is public? What can the provider do with the data? An LLC or trust does
not automatically add deletion rights. Oregon excludes commercial-context activity from its consumer definition.
Separation can improve exposure while changing the statutory rights available for a particular interaction.

Sources:

- [Or. Rev. Stat. § 646A.570][oregon-scope]

[oregon-scope]:
  <https://www.oregonlegislature.gov/bills_laws/ors/ors646a.html>

## V. Death and digital control

### Your digital successor

![A key passes between a person and a successor](img/delete-your-data/12-successor.png)

---

Oregon, California, and Washington provide routes for documented fiduciary requests to terminate accounts. Establish who
has authority; provide the required death certificate, authority documents, and account identification.
Account-termination authority does not create a universal right to erase the person's footprint.

Sources:

- [Or. Rev. Stat. § 119.062][death-or]
- [Cal. Prob. Code § 880][death-ca]
- [Wash. Rev. Code § 11.120.150][death-wa]
- [Cal. Prob. Code § 874][death-limits]

[death-or]:
  <https://www.oregonlegislature.gov/bills_laws/ors/ors119.html>
[death-ca]:
  <https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=PROB&sectionNum=880.>
[death-wa]:
  <https://app.leg.wa.gov/rcw/default.aspx?cite=11.120.150>
[death-limits]:
  <https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=PROB&sectionNum=874.>

### Make a private inventory

![Instructions connect accounts to a successor](img/delete-your-data/13-instructions.png)

---

List important accounts and their purpose in a private inventory. Record where the successor can find the relevant
authority documents and provider instructions. Keep credentials in an appropriate secure system, separate from public
estate filings.

### Leave clear instructions

![A person prepares illustrated instructions and a sealed key envelope](img/delete-your-data/art-19.png)

---

Inventory accounts. Name the appropriate fiduciary. Specify preserve, transfer, or delete. Align provider tools with the
estate documents: qualifying online disclosure directions can override conflicting will or trust instructions. Keep
credentials out of public documents. Access to message contents can require separate consent and documentation.

Sources:

- [California digital-asset directions][directions]
- [Google's deceased-user process][google-death]

[directions]:
  <https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=PROB&sectionNum=873.>
[google-death]:
  <https://support.google.com/accounts/troubleshooter/6357590?hl=EN>

### Decide what should survive

![Records move into an archive before closure](img/delete-your-data/14-preserve.png)

---

Choose preserve, transfer, or delete for each account. Some records support estate administration; some hold memories;
some are unnecessary exposure. Give the successor enough guidance to distinguish those purposes before closing anything.

### Preserve, then close

![An archivist preserves a keepsake before closing an empty cabinet](img/delete-your-data/art-20.png)

---

Preserve records needed for estate administration before deletion. Then use documented termination requests and provider
death procedures. Seek broker suppression separately. Google warns that closing an account prevents later delivery of
its contents. Do not promise removal of public records, reporting, or copies held by others.

Sources:

- [Google's deceased-user process][google-death]
- [California fiduciary limits][death-limits]

[google-death]:
  <https://support.google.com/accounts/troubleshooter/6357590?hl=EN>
[death-limits]:
  <https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=PROB&sectionNum=874.>

### Test the handoff

![Two adults rehearse a key handoff at a kitchen table](img/delete-your-data/art-21.png)

---

Check that the person you name knows the plan exists and can locate its instructions. Confirm the provider process and
revisit it when accounts or relationships change. A usable handoff requires both authority and a practical route.

## VI. State removal rights

### The deletion loop

![A screen recalls the original deletion talk](img/delete-your-data/01-original.png)

---

Find one exposed piece of information. Identify the organization holding it. Use its documented request channel and keep
the response. Treat discovery, request, and follow-up as three separate actions.

### Oregon

![An Oregon outline contains a record and a deletion check](img/delete-your-data/15-oregon.png)

---

For covered processing, request deletion of provided, purchased, and derived personal data. Also access, correct, and
opt out of sale, targeted advertising, and qualifying profiling. Coverage and exceptions matter. Public-record data is a
major limit. An ordinary request receives a response within 45 days, with a permitted extension in qualifying cases.

Presenter reference: ORS 646A.574(1)(c) addresses deletion of data supplied by the consumer, obtained from other
sources, and derived data. ORS 646A.576(1) addresses the request channel; (5)(a) addresses the 45-day response and
permitted extension; (5)(b) and (6) address refusals and appeals. Check coverage and exceptions before applying these
provisions. Midpage marks both sections current as of October 23, 2026.

Sources:

- [Or. Rev. Stat. §§ 646A.574 and 646A.576][oregon-delete]
- [Oregon consumer FAQs][oregon-faq]

[oregon-delete]:
  <https://www.oregonlegislature.gov/bills_laws/ors/ors646a.html>
[oregon-faq]:
  <https://www.doj.state.or.us/consumer-protection/id-theft-data-breaches/privacy/privacy-law-faqs-for-consumers/>

### California

![A California outline connects to several data brokers](img/delete-your-data/16-california.png)

---

CCPA: request deletion of personal information collected from you, subject to coverage and exceptions. DROP adds a
centralized request to registered data brokers; mandatory processing began August 1, 2026, with recurring checks. This
does not erase the internet. Recheck the effective dates of recent amendments before delivering the talk.

Presenter reference: Civil Code section 1798.105(a) states the deletion-request right. Subdivision (c)(1) addresses
deletion and notices to service providers, contractors, and recipients, subject to its stated limits. Subdivision (c)(2)
permits a limited confidential request record; subdivision (d) contains retention exceptions. These provisions alone do
not establish a private damages claim. Midpage marks this section current as of October 23, 2026.

Sources:

- [Midpage: Civil Code section 1798.105][california-midpage-0]
- [Cal. Civ. Code § 1798.105][california-delete]
- [CalPrivacy DROP regulations announcement][drop]

[california-delete]:
  <https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=CIV&sectionNum=1798.105.>
[drop]:
  <https://cppa.ca.gov/announcements/2025/20251113.html>

[california-midpage-0]:
  <https://app.midpage.ai/laws/ca-codes/civ/div3/pt4/t1%5F81%5F5/s1798%5F105-81e276>

### Washington

![A Washington outline contains a heart for consumer health data](img/delete-your-data/17-washington.png)

---

My Health My Data provides deletion rights for covered consumer health data and withdrawal of consent. Notice reaches
specified downstream recipients. Backup deletion can be delayed up to six months. This health-data route does not supply
a general right to remove every online record.

Presenter reference: RCW 19.373.040(1)(c) addresses deletion of consumer health data. Paragraphs (c)(i) and (ii) address
downstream notice and deletion; (c)(iii) allows the specified backup delay of up to six months after authentication.
Subsection (1)(g) sets the response period and extension; (1)(h) addresses appeals. This is a health-data law, not a
general internet-removal right. Midpage marks the section current as of October 23, 2026.

Sources:

- [Midpage: RCW 19.373.040][washington-midpage-0]
- [Wash. Rev. Code § 19.373.040][washington-delete]

[washington-delete]:
  <https://app.leg.wa.gov/rcw/default.aspx?cite=19.373.040>

[washington-midpage-0]:
  <https://app.midpage.ai/laws/wa-rcw/t19/c19%5F373/s19%5F373%5F040-7bf6c4>

### Choose the right request

![A barrier separates deletion from remaining copies](img/delete-your-data/18-boundaries.png)

---

Identify the holder and the kind of data first. Then choose account closure, a deletion request, search-result removal,
or broker suppression. Use the rights and provider process that fit that interaction. Keep the acknowledgement and
outcome together.

### Deletion has boundaries

![A librarian separates return items from protected archives](img/delete-your-data/art-22.png)

---

Separate account closure, source deletion, search removal, and broker suppression. Exceptions and required retention can
leave records behind. A successful request to one holder does not erase independent copies. Preserve the request and
response so the next action starts from evidence.

Sources:

- [California deletion exceptions][california-delete]
- [Washington downstream deletion][washington-delete]

[california-delete]:
  <https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=CIV&sectionNum=1798.105.>
[washington-delete]:
  <https://app.leg.wa.gov/rcw/default.aspx?cite=19.373.040>

### Keep the receipt

![A person files a receipt beside an envelope and clock](img/delete-your-data/art-23.png)

---

Keep a private log of the request date, recipient, scope, and response. Calendar any follow-up after checking the
applicable rule. If information reappears, the log shows the request and the confirmed outcome.

## VII. Data hygiene

### A smaller footprint

![A traveler leaves faint footprints along a forest path](img/delete-your-data/art-24.png)

---

Close unused accounts. Limit optional permissions. Use distinct aliases where supported. Review public contact details,
sharing, recovery paths, and business administrators. Keep a private request log and check for reappearance. These are
ongoing habits.

### Personal data stays close

![A person stores letters in a chest at home](img/delete-your-data/art-25.png)

---

Keep personal records out of business demos, shared prompts, and public profiles. Review connected tools and optional
sharing. Choosing not to upload something is often the simplest privacy decision available.

### A recurring privacy check

![A person tends a garden and checks its gate](img/delete-your-data/art-26.png)

---

Revisit old accounts, permissions, recovery paths, and public contact details on a regular schedule. Check whether
removed information returns. Update the private inventory when you add an account or change who can administer it.

### Choose one next step

![A person approaches an open door](img/delete-your-data/20-choice.png)

---

One exposed address. One unused account. One successor instruction. Reduce what gets published, request what can be
removed, and prepare the next person to act. Autonomy is the through-line.

## VIII. Delete Your Data: what we cover

### Delete Your Data services

![Delete Your Data trash-bin logo](/public/brand/delete-your-data/logo.svg)

---

Delete Your Data helps you request removal of personal information. Privacy, credit, and dark web monitoring help you
watch for signals that call for follow-up. Choose the monitoring you want to activate.

Monitoring helps identify issues; it does not prevent every misuse or data breach. Data removal is a non-legal service.
Legal advice and representation require a separate engagement.

### A death check you choose

![An older adult and trusted contact arrange a check-in over tea](img/delete-your-data/art-27.png)

---

If you have an estate plan with us, we agree on the actions to take after your death: who to notify, which claims to
help initiate, and which accounts to close. An optional death check helps set that agreed plan in motion while your
subscription is active.

Your instructions, the fiduciary's authority, provider requirements, and the written engagement determine the work. The
subscription does not itself appoint an executor or trustee.

### Help starting a company

![Two founders open a neighborhood workshop](img/delete-your-data/art-28.png)

---

The plan includes basic form-filing help, such as forming a company. Government filing fees are excluded. We identify
the requested filing and confirm the applicable scope before proceeding.

Describe the services we actually include. Custom legal advice, representation, and additional work need their own
agreed scope; do not promise every service another provider offers.

### Find unclaimed property

![An archivist returns a keepsake box](img/delete-your-data/unclaimed-property-ghibli.png)

---

We monitor official unclaimed-property websites and help you file claims for property that may belong to you. We help
you identify the relevant program, gather the required documents, and submit the claim.

The agency determines ownership and whether to approve the claim. Claims for someone who has died require the proper
authority and supporting records. Claim assistance does not guarantee recovery.

### Save the removal request

![An archivist files a sealed request in a private cabinet](img/delete-your-data/art-29.png)

---

Save the removal request, the version sent, its destination, the submission date, and any acknowledgement or response.
Keep supporting documents and any lawyer-review record in approved private storage.

This record lets us reconstruct what was requested and what the recipient said it did. Follow-up starts from that
record, including when the same information appears again.

### Record the lawyer review

![A lawyer reviews a document and records a separate abstract seal](img/delete-your-data/art-30.png)

---

The review record should identify the lawyer, the version reviewed, the time, and the scope of review. An optional
Solana attestation can link a document hash to that record while keeping the request and personal information private.

Solana recording is an on-request service; the automated backend is not yet shipped. Public metadata can still reveal
connections, so review what will be recorded with the person first.

If a later breach raises a legal issue, the retained documents and attestation can help establish the chronology and
which version was reviewed. They do not alone prove delivery, a duty to delete, noncompliance, causation, damages, or a
right to sue. A lawyer assesses the actual claim and the supporting evidence.

### Coming soon: private legal questions

![A person consults a lawyer through a secure booth](img/delete-your-data/art-31.png)

---

We plan to add protected screens in the web app where you can use LLMs to explore legal questions within an agreed legal
engagement. The planned service uses token-based pricing, with rates and any subscription inclusions disclosed before
use.

These screens are intended to support confidential requests for legal advice. Attorney-client privilege depends on the
relationship, purpose, participants, and handling of the communication; a screen or model response alone does not
establish privilege. The service needs appropriate access controls, provider terms, retention settings, and lawyer
supervision before launch. This feature is coming soon, not available through the current subscription.

Sources:

- [California State Bar guidance for generative AI][portal-ai-guidance]

[portal-ai-guidance]:
  <https://www.calbar.ca.gov/Portals/0/documents/ethics/Generative-AI-Practical-Guidance.pdf>

### $29/month, filing fees separate

![A legal service counter separates a coin jar from filing papers](img/delete-your-data/art-32.png)

---

Delete Your Data is $29 per month. Filing fees are separate.

Review the enrollment terms for the services currently included. The planned legal-question portal has token-based
pricing, with its rates and any included usage disclosed before use.

### More services, clear scope

![Caretakers inspect new rooms in a growing village workshop](img/delete-your-data/art-33.png)

---

We plan to add more services over time. The enrollment terms identify the services currently included, the price, and
any excluded fees. Government filing fees are separate.

Check the current offer before enrolling. Announce a new service when its scope, availability, and price are confirmed.

### Thanks for attending! BSidesPDX CTF key #1

![Scan to save CTF key #1: (5342, 511455802379373629457355)](/public/brand/delete-your-data/ctf-key-1-qr.svg)

Key #1: `(5342, 511455802379373629457355)`

Scan to save the key. Use it in the BSidesPDX CTF.

---

Leave this slide up so players can scan or copy the key. The QR code contains the exact pair printed on the slide.
Preserve both numbers and their order.

The CTF organizers approved sharing the key on the final slide. Key #1 and the pair above come from the organizer's
email supplied for this talk.
