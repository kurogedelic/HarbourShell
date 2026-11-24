/* SPDX-License-Identifier: MPL-2.0 */
/* Yacht's synthesizer: a small host for the Felucca-Engines (third_party/felucca-engines, MIT) that renders
 * a 16-step loop to mono 16-bit samples. Built by build.rs with the cc crate; called from yacht_synth.rs.
 *
 * One part, eight voices, a plain ADSR per control block (CTL = 32 samples) taken from the preset's
 * ATK DEC SUS REL. The loop is played twice and the second pass is kept, so the tails of the notes at the
 * end of the loop ring on into its start and the loop joins without a gap or a click.
 * The engines keep static state (WHEEL): the caller renders one loop at a time. */
#include <stdint.h>
#include <string.h>

#define NVOICE 8
#define NPART 1
#include "fe_tables.h"
#include "felucca_engine.h"

enum { P_ATK, P_REL, P_FTYPE, P_E0, P_E1, P_E2, P_E3, P_E4, P_E5, P_E6, P_E7, P_COUNT };
typedef struct track {
    int16_t p[P_COUNT];
    voice_t v[NVOICE];
} track_t;
static track_t trk[NPART];

#include "dsp.c"
#include "eng_analog.c"
#include "eng_lofi.c"
#include "eng_trio.c"
#include "eng_wheel.c"
#include "eng_noise.c"
#include "eng_phase.c"

static const engine_t *const ENGS[] = {&ENG_LOFI, &ENG_ANALOG, &ENG_TRIO, &ENG_PHASE, &ENG_WHEEL, &ENG_NOISE};

typedef struct {
    uint8_t step, pitch, len;
} ys_note_t;

uint32_t ys_rate(void) { return FS; }
uint32_t ys_block(void) { return CTL; }
uint32_t ys_engine_count(void) { return (uint32_t)NELEM(ENGS); }
const char *ys_engine_name(uint32_t e) { return e < NELEM(ENGS) ? ENGS[e]->name : ""; }
uint32_t ys_preset_count(uint32_t e) { return e < NELEM(ENGS) ? ENGS[e]->npresets : 0; }
const char *ys_preset_name(uint32_t e, uint32_t p)
{
    return e < NELEM(ENGS) && p < ENGS[e]->npresets ? ENGS[e]->presets[p].name : "";
}

/* ---------------------------------------------------------------- voices --- */
static uint32_t vage;

static void voice_on(track_t *t, const engine_t *e, voice_t *v, uint32_t note)
{
    int sounding = v->active && v->stage != 0;
    uint32_t ph0 = v->ph[0], ph1 = v->ph[1], ph2 = v->ph[2];
    int32_t s[8];
    memcpy(s, v->s, sizeof s);
    v->note = (uint8_t)note;
    v->vel = 100;
    v->gate = 1;
    v->active = 1;
    v->stage = 1;
    v->age = ++vage;
    v->pitch16 = v->pitch_cur = (int32_t)note * 16;
    v->fine = 0;
    if (!sounding)
        v->env = v->env_out = 0;
    e->note_on(t, v);
    if (sounding) {
        uint32_t k;
        v->ph[0] = ph0;
        v->ph[1] = ph1;
        v->ph[2] = ph2;
        for (k = 0; k < 8u; k++)
            if (k != 2u && k != 3u && (e->keep >> k & 1u))
                v->s[k] = s[k];
    }
}

/* Blocks for a time value 0..127: 0 = one block, 127 = about two seconds (quadratic, like a knob). */
static int32_t env_blocks(int32_t x)
{
    int32_t b = 1 + (x * x * (FS / CTL) * 2) / (127 * 127);
    return b < 1 ? 1 : b;
}

static int32_t env_tick(voice_t *v, const preset_t *pr)
{
    const int32_t full = 1 << 24, sus = (int32_t)(((int64_t)full * pr->env[2]) / 127);
    if (!v->active)
        return 0;
    switch (v->stage) {
    case 1:
        v->env += full / env_blocks(pr->env[0]);
        if (v->env >= full) {
            v->env = full;
            v->stage = 2;
        }
        break;
    case 2: {
        int32_t d = (full - sus) / env_blocks(pr->env[1]);
        v->env = v->env - d > sus ? v->env - d : sus;
        break;
    }
    case 3: {
        int32_t d = full / env_blocks(pr->env[3]);
        v->env -= d > 1 ? d : 1;
        if (v->env <= 0) {
            v->env = 0;
            v->stage = 0;
            v->active = 0;
        }
        break;
    }
    default:
        break;
    }
    return v->env >> 9;                                 /* Q15 */
}

static voice_t *voice_for(track_t *t, const engine_t *e, const preset_t *pr, uint32_t note)
{
    uint32_t k, cap = e->poly && e->poly < NVOICE ? e->poly : NVOICE;
    voice_t *best = &t->v[0];
    if (pr->mono)
        return &t->v[0];
    for (k = 0; k < cap; k++)                           /* the same note again: retrigger it */
        if (t->v[k].active && t->v[k].note == note)
            return &t->v[k];
    for (k = 0; k < cap; k++)
        if (!t->v[k].active)
            return &t->v[k];
    for (k = 1; k < cap; k++)                           /* all busy: the oldest */
        if (t->v[k].age < best->age)
            best = &t->v[k];
    return best;
}

/* Render `steps` steps of `step_len` samples each (a multiple of CTL) into out (steps * step_len samples).
 * Returns 0, or -1 for a bad engine / preset / length. */
int32_t ys_render(uint32_t eng, uint32_t preset, const ys_note_t *notes, uint32_t nnotes, uint32_t step_len,
                  uint32_t steps, int16_t *out)
{
    const engine_t *e;
    const preset_t *pr;
    track_t *t = &trk[0];
    int32_t buf[CTL];
    int32_t last[NVOICE] = {0};
    voice_t *held[256] = {0};                           /* the voice each note plays on */
    uint32_t pass, s, b, i, k, n, bps;
    if (eng >= NELEM(ENGS) || step_len == 0 || step_len % CTL || nnotes > 256)
        return -1;
    e = ENGS[eng];
    if (preset >= e->npresets)
        return -1;
    pr = &e->presets[preset];
    bps = step_len / CTL;

    memset(t, 0, sizeof *t);
    memset(drw_t, 0, sizeof drw_t);
    memset(drw_v, 0, sizeof drw_v);
    vage = 0;
    for (k = 0; k < 8u; k++)
        t->p[P_E0 + k] = pr->e[k];
    t->p[P_ATK] = pr->env[0];
    t->p[P_REL] = pr->env[3];
    t->p[P_FTYPE] = 0;

    for (pass = 0; pass < 2; pass++) {
        for (s = 0; s < steps; s++) {
            /* note-offs first (a note ending here), then the notes starting on this step */
            for (n = 0; n < nnotes; n++) {
                voice_t *v = held[n];
                if (v && (uint32_t)notes[n].step + notes[n].len == s + (s == 0 ? steps : 0) && v->note == notes[n].pitch &&
                    v->gate) {
                    v->gate = 0;
                    v->stage = 3;
                    held[n] = 0;
                }
            }
            for (n = 0; n < nnotes; n++)
                if (notes[n].step == s) {
                    voice_t *v = voice_for(t, e, pr, notes[n].pitch);
                    voice_on(t, e, v, notes[n].pitch);
                    held[n] = v;
                }
            for (b = 0; b < bps; b++) {
                if (e->block)
                    e->block(t);
                memset(buf, 0, sizeof buf);
                for (k = 0; k < NVOICE; k++) {
                    voice_t *v = &t->v[k];
                    vmod_t m;
                    int32_t a;
                    uint32_t base;
                    if (!v->active)
                        continue;
                    a = env_tick(v, pr);
                    if (e->amp)
                        a = e->amp(t, v, a);
                    if (!v->active)
                        continue;
                    base = pitch_inc((uint32_t)clamp(v->pitch16, 0, 2047));
                    memset(&m, 0, sizeof m);
                    m.pitch16 = v->pitch16;
                    m.inc = base;
                    m.amp0 = last[k];
                    m.amp1 = a;
                    m.envq15 = a;
                    m.cutoff = 0;
                    m.shape = 64 << 8;
                    last[k] = a;
                    e->render(t, v, buf, CTL, &m);
                }
                for (k = 0; k < NVOICE; k++)
                    if (!t->v[k].active)
                        last[k] = 0;
                if (e->post)
                    e->post(t, buf, CTL);
                if (pass == 1) {
                    int16_t *o = out + (size_t)s * step_len + (size_t)b * CTL;
                    for (i = 0; i < CTL; i++)
                        o[i] = (int16_t)clamp(buf[i] / 3, -32767, 32767);
                }
            }
        }
    }
    return 0;
}
