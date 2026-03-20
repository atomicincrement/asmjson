# JOSS Pre-flight Checklist

## Paper

| Item | Status |
|---|---|
| Prose word count (750–1750) | ✅ 1501 |
| Summary (required section) | ✅ |
| Statement of need (required section) | ✅ |
| State of the field (required section) | ✅ |
| Software design (required section) | ✅ |
| Research impact statement (required section) | ✅ |
| AI usage disclosure (required section) | ✅ |
| Acknowledgements | ✅ |
| References / paper.bib | ✅ |
| Author affiliations | ✅ |
| ORCID | ✅ `0000-0001-8240-1614` |
| Date format (`%e %B %Y`) | ✅ `20 March 2026` |
| All in-text citations have bib entries | ✅ |

## Repository

| Item | Status |
|---|---|
| License (OSI-approved) | ✅ MIT — `LICENSE` |
| Automated tests | ✅ `cargo test` |
| CI badge in README | ✅ GitHub Actions |
| Installation instructions | ✅ README Cargo.toml snippet |
| Example usage | ✅ README quick-start + examples/ |
| API documentation | ✅ docs.rs |
| Community guidelines (contribute / report / support) | ✅ `CONTRIBUTING.md` |

## Review checklist items (for reference)

From <https://joss.readthedocs.io/en/latest/review_checklist.html>.

### General checks
- [ ] Source code available at repository URL
- [ ] Plain-text OSI-approved LICENSE file present
- [ ] Submitting author has made major contributions
- [ ] Submission demonstrates clear research impact or credible scholarly significance

### Development history and open-source practice
- [ ] Evidence of sustained development over time
- [ ] Software developed openly from early stages
- [ ] Good open-source practices: license, docs, tests, releases, contribution pathways

### Functionality
- [ ] Installation proceeds as documented
- [ ] Functional claims confirmed
- [ ] Performance claims confirmed (benchmarks reproducible via `cargo bench`)

### Documentation
- [ ] Statement of need present
- [ ] Installation instructions clear
- [ ] Example usage provided
- [ ] Core functionality documented (API docs)
- [ ] Automated tests present
- [ ] Community guidelines present

### Software paper
- [ ] Summary section present and accessible to non-specialists
- [ ] Statement of need section present
- [ ] State of the field section present
- [ ] Software design section present with meaningful design thinking
- [ ] Research impact statement present with compelling evidence
- [ ] AI usage disclosure present
- [ ] Writing quality acceptable
- [ ] References complete and correctly cited
