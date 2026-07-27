// hwrig-mcu -- control MCU for the Copperline hardware reference rig.
//
// Sits on the Amiga's keyboard connector and gives the host two things the
// serial probe link cannot provide: a way to type, and a way to recover a
// machine a probe has wedged. Commands arrive as lines on the USB serial port.
//
// Target: Arduino Uno (ATmega328P, 16 MHz, 5V).
//
// The MCU must be natively 5V. KCLK, KDAT and /RESET are held at +5V by
// pull-ups on the Amiga side, so they idle high at 5V. A 3.3V part such as the
// RP2040 is not 5V tolerant and would clamp 5V into its supply rail through the
// input protection diodes even with the pin in hi-Z. A Leonardo/Pro Micro
// (ATmega32U4, 5V/16MHz -- NOT the 3.3V/8MHz variant) also works and has native
// USB. The protocol here is 20us-scale with 143ms timeouts, so a 16MHz AVR has
// enormous margin; nothing here needs a faster part.
//
// The keyboard protocol below follows the maintainer's own A500KBFirmware
// (common/common.c send_data / sync_with_computer) rather than a reading of the
// HRM, so the bit order, inversion and handshake match hardware that is known
// to work on these machines.

#include <avr/wdt.h>

// ---------------------------------------------------------------- wiring
// See README.md for the connector pinout and the ground/power rules. Take the
// pin numbers on the Amiga side from your own adapter or the machine's
// schematic -- they differ between the A500's internal header and the big-box
// 5-pin DIN.
static const uint8_t PIN_KCLK  = 2;   // Amiga keyboard clock
static const uint8_t PIN_KDAT  = 3;   // Amiga keyboard data
static const uint8_t PIN_RESET = 4;   // /RESET, open drain, active low
static const uint8_t PIN_RELAY = 5;   // optional PSU relay for cold boot
static const uint8_t PIN_LED   = LED_BUILTIN;

// Relay module polarity. Most cheap opto-isolated boards are active low.
static const bool RELAY_ACTIVE_LOW = true;

// ---------------------------------------------------------------- protocol
static const uint8_t KEY_RESYNC     = 0xF9;
static const uint8_t KEY_INIT_START = 0xFD;
static const uint8_t KEY_INIT_END   = 0xFE;

static bool synced = false;

// --- line primitives -------------------------------------------------------
// KCLK and KDAT are actively driven while the keyboard is transmitting; only
// KDAT is released, for the Amiga's acknowledge pulse. /RESET is different: it
// is a wired-OR line with other drivers on it and is only ever pulled low or
// released, never driven high.
static inline void driveHigh(uint8_t pin) {
    digitalWrite(pin, HIGH);
    pinMode(pin, OUTPUT);
}

static inline void driveLow(uint8_t pin) {
    digitalWrite(pin, LOW);
    pinMode(pin, OUTPUT);
}

static inline void release(uint8_t pin) {
    pinMode(pin, INPUT);          // hi-Z; the Amiga's pull-up takes it high
    digitalWrite(pin, LOW);       // never enable the internal pull-up
}

// --- keyboard transmit -----------------------------------------------------
// One keycode. The byte is rotated left by one so bit 7 (the up/down flag) is
// sent last, and the data is inverted on the wire: a 1 bit pulls KDAT low.
// Returns false if the Amiga never acknowledged within 143ms, which means the
// link needs resyncing.
static bool amigaSend(uint8_t code) {
    driveHigh(PIN_KCLK);
    driveHigh(PIN_KDAT);

    uint8_t b = (uint8_t)((code >> 7) | (code << 1));
    delayMicroseconds(2);

    for (uint8_t i = 0; i < 8; i++) {
        if (b & 0x80) {
            digitalWrite(PIN_KDAT, LOW);     // inverted: a 1 bit drives low
        } else {
            digitalWrite(PIN_KDAT, HIGH);
        }
        delayMicroseconds(20);
        digitalWrite(PIN_KCLK, LOW);
        delayMicroseconds(20);
        digitalWrite(PIN_KCLK, HIGH);
        delayMicroseconds(20);
        b <<= 1;
    }

    digitalWrite(PIN_KDAT, HIGH);
    delayMicroseconds(2);
    release(PIN_KDAT);

    // Wait up to 143ms for the acknowledge: KDAT pulled low, then released.
    bool pulsedLow = false;
    for (uint16_t i = 0; i < 28600; i++) {
        wdt_reset();
        if (digitalRead(PIN_KDAT) == LOW) {
            pulsedLow = true;
        } else if (pulsedLow) {
            return true;
        }
        delayMicroseconds(5);
    }
    return false;
}

// Handshake the Amiga into a known state: one clock pulse with KDAT held low,
// then wait for the acknowledge. Must succeed before any keycode is believed.
static bool amigaSync() {
    digitalWrite(PIN_KDAT, LOW);
    digitalWrite(PIN_KCLK, HIGH);
    delayMicroseconds(20);
    pinMode(PIN_KCLK, OUTPUT);
    pinMode(PIN_KDAT, OUTPUT);
    delayMicroseconds(20);
    digitalWrite(PIN_KCLK, LOW);
    delayMicroseconds(20);
    digitalWrite(PIN_KCLK, HIGH);
    delayMicroseconds(20);
    release(PIN_KDAT);
    delayMicroseconds(5);

    bool pulsedLow = false;
    for (uint32_t i = 0; i < 143000; i++) {
        wdt_reset();
        if (digitalRead(PIN_KDAT) == LOW) { pulsedLow = true; break; }
        delayMicroseconds(1);
    }
    if (!pulsedLow) return false;

    // Hold until the Amiga lets KDAT go again, bounded so a machine sitting
    // with the line stuck low cannot hang the MCU.
    for (uint32_t i = 0; i < 200000; i++) {
        wdt_reset();
        if (digitalRead(PIN_KDAT) != LOW) return true;
        delayMicroseconds(50);
    }
    return false;
}

// The power-on stream a real keyboard sends: "here is the start of the keys
// that are held down", nothing, "that is all of them".
static bool amigaHello() {
    if (!amigaSync()) return false;
    if (!amigaSend(KEY_INIT_START)) return false;
    if (!amigaSend(KEY_INIT_END)) return false;
    return true;
}

// --- reset paths -----------------------------------------------------------
// Hard reset on the /RESET line. Open drain and bounded in firmware: if the
// host dies mid-command the pulse still ends and the Amiga is never left held
// in reset.
static void hardReset() {
    driveLow(PIN_RESET);
    for (uint8_t i = 0; i < 10; i++) { wdt_reset(); delay(50); }
    release(PIN_RESET);
    synced = false;
}

// Keyboard-initiated reset: the Ctrl-A-A path, KCLK held low for >= 500ms.
// Uses no wire the keyboard connector does not already have.
static void keyboardReset() {
    driveLow(PIN_KCLK);
    for (uint8_t i = 0; i < 12; i++) { wdt_reset(); delay(50); }
    release(PIN_KCLK);
    synced = false;
}

static void relaySet(bool on) {
    bool level = RELAY_ACTIVE_LOW ? !on : on;
    digitalWrite(PIN_RELAY, level ? HIGH : LOW);
    pinMode(PIN_RELAY, OUTPUT);
}

// Cold boot. Distinct from a reset: power-on state is observable from the guest
// (uninitialised chip RAM shows up on screen), so probes that care about it
// need the real thing.
static void powerCycle() {
    relaySet(false);
    for (uint8_t i = 0; i < 60; i++) { wdt_reset(); delay(50); }   // 3s off
    relaySet(true);
    synced = false;
}

// --- command parsing -------------------------------------------------------
static bool parseHex(const char *s, uint8_t *out) {
    uint16_t v = 0;
    uint8_t n = 0;
    for (; *s; s++) {
        char c = *s;
        uint8_t d;
        if (c >= '0' && c <= '9')      d = c - '0';
        else if (c >= 'a' && c <= 'f') d = c - 'a' + 10;
        else if (c >= 'A' && c <= 'F') d = c - 'A' + 10;
        else return false;
        v = (uint16_t)(v << 4) | d;
        if (++n > 2) return false;
    }
    if (n == 0) return false;
    *out = (uint8_t)v;
    return true;
}

// Send one keycode, resyncing once if the Amiga did not acknowledge. A machine
// that has just been reset always needs this.
static bool sendKeyRetry(uint8_t code) {
    if (!synced) {
        if (!amigaHello()) return false;
        synced = true;
    }
    if (amigaSend(code)) return true;
    synced = false;
    if (!amigaHello()) return false;
    synced = true;
    if (!amigaSend(KEY_RESYNC)) return false;
    return amigaSend(code);
}

static void handleCommand(char *line) {
    char *arg = strchr(line, ' ');
    if (arg) { *arg++ = '\0'; while (*arg == ' ') arg++; }

    for (char *p = line; *p; p++) {
        if (*p >= 'a' && *p <= 'z') *p -= 32;
    }

    if (!strcmp(line, "ID")) {
        Serial.println(F("OK hwrig-mcu 1 uno"));
    } else if (!strcmp(line, "PING")) {
        Serial.println(F("OK pong"));
    } else if (!strcmp(line, "SYNC")) {
        synced = amigaHello();
        Serial.println(synced ? F("OK sync") : F("ERR sync: no acknowledge"));
    } else if (!strcmp(line, "RESET")) {
        hardReset();
        Serial.println(F("OK reset"));
    } else if (!strcmp(line, "CAA")) {
        keyboardReset();
        Serial.println(F("OK caa"));
    } else if (!strcmp(line, "POWER")) {
        powerCycle();
        Serial.println(F("OK power"));
    } else if (!strcmp(line, "KEY") || !strcmp(line, "DOWN") ||
               !strcmp(line, "UP")) {
        uint8_t code;
        if (!arg || !parseHex(arg, &code)) {
            Serial.println(F("ERR bad keycode"));
            return;
        }
        bool ok;
        if (!strcmp(line, "DOWN")) {
            ok = sendKeyRetry(code & 0x7F);
        } else if (!strcmp(line, "UP")) {
            ok = sendKeyRetry(code | 0x80);
        } else {
            ok = sendKeyRetry(code & 0x7F);
            if (ok) { delay(40); ok = sendKeyRetry(code | 0x80); }
        }
        Serial.println(ok ? F("OK key") : F("ERR key: no acknowledge"));
    } else {
        Serial.println(F("ERR unknown command"));
    }
}

// --- main ------------------------------------------------------------------
void setup() {
    // Everything starts released, so a power-on or a watchdog reset of the MCU
    // can never assert /RESET or hold a keyboard line down.
    release(PIN_KCLK);
    release(PIN_KDAT);
    release(PIN_RESET);
    relaySet(true);
    pinMode(PIN_LED, OUTPUT);
    digitalWrite(PIN_LED, LOW);

    Serial.begin(19200);
    wdt_enable(WDTO_8S);

    // Do not announce ourselves yet: the Amiga may not be up. The first KEY
    // command syncs on demand, and SYNC forces it.
    Serial.println(F("OK hwrig-mcu 1 uno ready"));
}

void loop() {
    static char line[64];
    static uint8_t len = 0;

    wdt_reset();
    while (Serial.available()) {
        char c = (char)Serial.read();
        if (c == '\r') continue;
        if (c == '\n') {
            line[len] = '\0';
            if (len) {
                digitalWrite(PIN_LED, HIGH);
                handleCommand(line);
                digitalWrite(PIN_LED, LOW);
            }
            len = 0;
        } else if (len < sizeof(line) - 1) {
            line[len++] = c;
        }
        wdt_reset();
    }
}
