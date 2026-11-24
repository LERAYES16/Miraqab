/*
 * Test helper for Miraqab's end-to-end tests (NOT shipped to users).
 *
 * Creates a real X11 window with an animated pattern and resizes it on a
 * schedule, so that window capture (x11grab -window_id) and dynamic resizing
 * can be exercised against a real X server (Xvfb).
 *
 * usage: xwin WxH [T:WxH ...] DURATION_SECONDS
 *   prints the window id (0x...) on the first line of stdout, then animates
 *   until DURATION_SECONDS have passed.  "T:WxH" resizes the window to WxH
 *   when T seconds have elapsed.
 */
#include <X11/Xlib.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "usage: xwin WxH [T:WxH ...] DURATION\n");
        return 2;
    }
    int w = 0, h = 0;
    if (sscanf(argv[1], "%dx%d", &w, &h) != 2) return 2;
    double duration = atof(argv[argc - 1]);

    struct { double t; int w, h; } steps[16];
    int nsteps = 0;
    for (int i = 2; i < argc - 1 && nsteps < 16; i++) {
        double t; int sw, sh;
        if (sscanf(argv[i], "%lf:%dx%d", &t, &sw, &sh) == 3) {
            steps[nsteps].t = t; steps[nsteps].w = sw; steps[nsteps].h = sh;
            nsteps++;
        }
    }

    Display *dpy = XOpenDisplay(NULL);
    if (!dpy) { fprintf(stderr, "xwin: cannot open display\n"); return 1; }
    int scr = DefaultScreen(dpy);
    Window win = XCreateSimpleWindow(dpy, RootWindow(dpy, scr), 0, 0, w, h, 0, 0, 0x202020);
    XStoreName(dpy, win, "Miraqab Test Window");
    XSelectInput(dpy, win, ExposureMask);
    XMapWindow(dpy, win);
    XFlush(dpy);
    printf("0x%lx\n", win);
    fflush(stdout);

    GC gc = XCreateGC(dpy, win, 0, NULL);
    double t0 = now();
    int next = 0, cw = w, ch = h;
    unsigned long tick = 0;
    while (now() - t0 < duration) {
        double t = now() - t0;
        if (next < nsteps && t >= steps[next].t) {
            cw = steps[next].w; ch = steps[next].h;
            XResizeWindow(dpy, win, (unsigned)cw, (unsigned)ch);
            next++;
        }
        while (XPending(dpy)) { XEvent ev; XNextEvent(dpy, &ev); }
        /* left half red, right half blue, moving white bar */
        XSetForeground(dpy, gc, 0xC02020);
        XFillRectangle(dpy, win, gc, 0, 0, (unsigned)(cw / 2), (unsigned)ch);
        XSetForeground(dpy, gc, 0x2020C0);
        XFillRectangle(dpy, win, gc, cw / 2, 0, (unsigned)(cw - cw / 2), (unsigned)ch);
        XSetForeground(dpy, gc, 0xFFFFFF);
        int span = cw > 60 ? cw - 40 : 1;
        int bx = (int)((tick * 13) % (unsigned long)span);
        XFillRectangle(dpy, win, gc, bx, ch / 2 - 10, 40, 20);
        XFlush(dpy);
        tick++;
        usleep(50000);
    }
    XDestroyWindow(dpy, win);
    XCloseDisplay(dpy);
    return 0;
}
