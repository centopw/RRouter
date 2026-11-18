import { useId } from "react";
import { cx } from "../lib/cx";
import { useReducedMotion } from "../lib/media";

export type BrandMarkState = "idle" | "dispatch" | "burst" | "failover" | "error";

/**
 * The animated RRouter mark from the brand guide.
 *
 * The SVG is self-contained: unique gradient/clip/filter ids and prefixed animation
 * classes so the sidebar, auth screens and the live-routing hub can render it at the
 * same time without sharing ids or restarting each other's animations.
 */
export function BrandMark({
  variant = "compact",
  state = "idle",
  angleDeg = 0,
  color,
  surface,
  decorative = false,
  className,
  ariaLabel = "RRouter — One endpoint. Smarter routing.",
}: {
  variant?: "full" | "compact";
  state?: BrandMarkState;
  /** Rotates the switch arc toward the provider the gateway last dispatched to. */
  angleDeg?: number;
  /** Egress colour for the switch arc; defaults to the mark's cyan. */
  color?: string;
  /** Pin to light/dark palette; omit to follow the active theme tokens. */
  surface?: "light" | "dark";
  decorative?: boolean;
  className?: string;
  ariaLabel?: string;
}) {
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const reducedMotion = useReducedMotion();
  const p = `rrb${uid}`;
  const id = (name: string) => `${p}-${name}`;
  const cls = (name: string) => `${p}-${name}`;
  const arc =
    state === "error"
      ? "var(--color-neg)"
      : state === "failover"
        ? "var(--color-pos)"
        : color ?? "var(--rr-brand-cyan)";

  const staticCss = `
    .${cls("s-top")},.${cls("s-bot")},.${cls("s-mid")},.${cls("r-body")},.${cls("r-leg")},.${cls("dg")},.${cls("t1")},.${cls("t2")},.${cls("dot")}{opacity:1;transform:none;animation:none}
    #${id("logo")}[data-state="idle"] .${cls("s-top")},
    #${id("logo")}[data-state="idle"] .${cls("s-bot")}{opacity:.78}
    .${cls("spk")},.${cls("tip-glow")},.${cls("pk-top")},.${cls("pk-mid")},.${cls("pk-bot")},.${cls("r-glint")},.${cls("wm-glint")}{opacity:0;animation:none}
  `;

  const animatedCss = `
    .${cls("s-top")}{opacity:0;transform-box:fill-box;transform-origin:100% 50%;animation:sIn .52s cubic-bezier(.22,1.25,.36,1) 0s forwards}
    .${cls("s-bot")}{opacity:0;transform-box:fill-box;transform-origin:100% 50%;animation:sIn .52s cubic-bezier(.22,1.25,.36,1) .09s forwards}
    .${cls("s-mid")}{opacity:0;transform-box:fill-box;transform-origin:100% 50%;animation:aIn .58s cubic-bezier(.22,1.28,.36,1) .18s forwards}
    .${cls("r-body")}{opacity:0;transform-box:fill-box;transform-origin:32% 48%;animation:rUnfurl .56s cubic-bezier(.22,1.24,.36,1) .44s forwards}
    .${cls("r-leg")}{opacity:0;transform-box:fill-box;transform-origin:18% 8%;animation:rLeg .52s cubic-bezier(.34,1.56,.64,1) .56s forwards}
    .${cls("r-react")}{transform-box:fill-box;transform-origin:48% 72%;animation:rNod .56s cubic-bezier(.34,1.56,.64,1) .58s,rNodLoop 6s cubic-bezier(.34,1.56,.64,1) 3s infinite}
    .${cls("spk")}{opacity:0;transform-box:fill-box;transform-origin:50% 50%}
    .${cls("spk-a")}{animation:spA .54s cubic-bezier(.16,1,.3,1) .6s forwards,spALoop 6s cubic-bezier(.16,1,.3,1) 3.02s infinite}
    .${cls("spk-b")}{animation:spB .54s cubic-bezier(.16,1,.3,1) .62s forwards,spBLoop 6s cubic-bezier(.16,1,.3,1) 3.04s infinite}
    .${cls("spk-c")}{animation:spC .54s cubic-bezier(.16,1,.3,1) .64s forwards,spCLoop 6s cubic-bezier(.16,1,.3,1) 3.06s infinite}
    .${cls("spk-d")}{opacity:0}
    .${cls("dg")}{opacity:0;transform-box:fill-box;transform-origin:50% 88%;animation:gHop .54s cubic-bezier(.22,1.32,.36,1) forwards}
    .${cls("dg1")}{animation-delay:.72s}.${cls("dg2")}{animation-delay:.765s}.${cls("dg3")}{animation-delay:.81s}.${cls("dg4")}{animation-delay:.855s}.${cls("dg5")}{animation-delay:.9s}.${cls("dg6")}{animation-delay:.945s}.${cls("dg7")}{animation-delay:.99s}
    .${cls("t1")}{opacity:0;animation:tGlide .48s cubic-bezier(.22,1,.36,1) 1.12s forwards}
    .${cls("t2")}{opacity:0;transform-box:fill-box;transform-origin:0% 80%;animation:tSpring .52s cubic-bezier(.22,1.24,.36,1) 1.28s forwards}
    .${cls("dot")}{opacity:0;transform-box:fill-box;transform-origin:50% 100%;animation:dotIn .64s cubic-bezier(.22,1,.36,1) 1.44s forwards,dotLoop 6s cubic-bezier(.34,1.56,.64,1) 3.22s infinite}
    .${cls("pk-top")}{transform:translateX(-110px);animation:pkT 6s cubic-bezier(.35,0,.15,1) 2.5s infinite}
    .${cls("pk-mid")}{transform:translateX(-125px);animation:pkM 6s cubic-bezier(.35,0,.15,1) 2.62s infinite}
    .${cls("pk-bot")}{transform:translateX(-95px);animation:pkB 6s cubic-bezier(.35,0,.15,1) 2.74s infinite}
    .${cls("a-nudge")}{transform-box:fill-box;transform-origin:60% 50%;animation:aNudge 6s cubic-bezier(.34,1.56,.64,1) 2.96s infinite}
    .${cls("tip-glow")}{opacity:0;animation:tGlow 6s ease-in-out 2.98s infinite}
    .${cls("r-glint")}{transform:translateX(-200px) skewX(-24deg);animation:rGlint 6s cubic-bezier(.25,.1,.25,1) 3.04s infinite}
    .${cls("wm-glint")}{transform:translateX(-200px) skewX(-24deg);animation:wmGlint 6s cubic-bezier(.25,.1,.25,1) 3.2s infinite}

    #${id("logo")}[data-state="idle"] .${cls("s-top")},
    #${id("logo")}[data-state="idle"] .${cls("s-bot")}{opacity:.78}
    #${id("logo")}[data-state="burst"] .${cls("pk-top")}{animation-duration:1.4s;animation-delay:0s}
    #${id("logo")}[data-state="burst"] .${cls("pk-mid")}{animation-duration:1.4s;animation-delay:.09s}
    #${id("logo")}[data-state="burst"] .${cls("pk-bot")}{animation-duration:1.4s;animation-delay:.18s}
    #${id("logo")}[data-state="dispatch"] .${cls("a-nudge")}{animation:dispatchNudge .9s cubic-bezier(.34,1.56,.64,1)}
    #${id("logo")}[data-state="dispatch"] .${cls("tip-glow")}{animation:dispatchGlow .9s ease-out}
    #${id("logo")}[data-state="error"] .${cls("r-react")}{animation:errShake .36s ease-in-out}
    #${id("logo")}[data-state="failover"] .${cls("r-react")}{animation:errShake .36s ease-in-out,failoverNod 1.1s cubic-bezier(.34,1.56,.64,1) .18s}
    #${id("logo")}[data-state="failover"] .${cls("s-bot")}{animation:failoverPulse 1.1s ease-in-out}
    #${id("logo")}[data-state="failover"] .${cls("spk-d")}{animation:failoverSpark 1.1s cubic-bezier(.16,1,.3,1)}

    @keyframes sIn{0%{opacity:0;transform:translateX(-68px) scaleX(.78)}68%{opacity:1;transform:translateX(4px) scaleX(1.015)}100%{opacity:1;transform:none}}
    @keyframes aIn{0%{opacity:0;transform:translateX(-84px) scaleX(.76)}68%{opacity:1;transform:translateX(6px) scaleX(1.025)}100%{opacity:1;transform:none}}
    @keyframes rUnfurl{0%{opacity:0;transform:translateX(-24px) scale(.84) rotate(-5deg)}68%{opacity:1;transform:translateX(3px) scale(1.02) rotate(1deg)}100%{opacity:1;transform:none}}
    @keyframes rLeg{0%{opacity:0;transform:rotate(-22deg) scale(.76)}70%{opacity:1;transform:rotate(3deg) scale(1.02)}100%{opacity:1;transform:none}}
    @keyframes rNod{0%,100%{transform:none}32%{transform:translateX(5px) rotate(2deg) scale(.978,1.02)}68%{transform:translateX(-1.5px) rotate(-.6deg)}}
    @keyframes rNodLoop{0%,100%{transform:none}5%{transform:translateX(2.5px) rotate(1deg)}14%{transform:none}}
    @keyframes spA{0%{opacity:0;transform:scale(.2)}30%{opacity:1;transform:translate(11px,-14px) scale(1.1)}100%{opacity:0;transform:translate(22px,-26px) scale(.25)}}
    @keyframes spB{0%{opacity:0;transform:scale(.2)}30%{opacity:1;transform:translate(16px,-2px) scale(1.15)}100%{opacity:0;transform:translate(30px,-4px) scale(.25)}}
    @keyframes spC{0%{opacity:0;transform:scale(.2)}30%{opacity:1;transform:translate(12px,13px) scale(1.1)}100%{opacity:0;transform:translate(23px,24px) scale(.25)}}
    @keyframes spALoop{0%,100%{opacity:0}4%{opacity:.85;transform:translate(9px,-11px)}15%{opacity:0;transform:translate(18px,-21px) scale(.2)}}
    @keyframes spBLoop{0%,100%{opacity:0}4%{opacity:.9;transform:translate(13px,-2px)}15%{opacity:0;transform:translate(25px,-4px) scale(.2)}}
    @keyframes spCLoop{0%,100%{opacity:0}4%{opacity:.85;transform:translate(10px,11px)}15%{opacity:0;transform:translate(20px,20px) scale(.2)}}
    @keyframes gHop{0%{opacity:0;transform:translateY(15px) scale(1.08,.86)}56%{opacity:1;transform:translateY(-5px) scale(.97,1.04)}100%{opacity:1;transform:none}}
    @keyframes tGlide{0%{opacity:0;transform:translateX(-12px)}100%{opacity:1;transform:none}}
    @keyframes tSpring{0%{opacity:0;transform:translateX(-12px) scale(.96)}72%{opacity:1;transform:translateX(2.5px) scale(1.01)}100%{opacity:1;transform:none}}
    @keyframes dotIn{0%{opacity:0;transform:translateY(-20px) scale(.88,1.12)}38%{opacity:1;transform:scale(1.24,.76)}66%{transform:translateY(-8px) scale(.94,1.07)}100%{opacity:1;transform:none}}
    @keyframes dotLoop{0%,34%,100%{transform:none}6%{transform:scale(1.2,.8)}14%{transform:translateY(-8px) scale(.94,1.08)}22%{transform:scale(1.1,.9)}}
    @keyframes pkT{0%{transform:translateX(-100px);opacity:0}4%{opacity:1}16%,100%{transform:translateX(295px);opacity:0}}
    @keyframes pkM{0%{transform:translateX(-115px);opacity:0}4%{opacity:1}18%,100%{transform:translateX(340px);opacity:0}}
    @keyframes pkB{0%{transform:translateX(-90px);opacity:0}4%{opacity:1}15%,100%{transform:translateX(230px);opacity:0}}
    @keyframes aNudge{0%,20%,100%{transform:none}4%{transform:translateX(3.5px)}12%{transform:translateX(-.8px)}}
    @keyframes tGlow{0%,18%,100%{opacity:0}5%{opacity:.45}}
    @keyframes rGlint{0%{transform:translateX(-180px) skewX(-24deg)}18%,100%{transform:translateX(480px) skewX(-24deg)}}
    @keyframes wmGlint{0%{transform:translateX(-180px) skewX(-24deg)}24%,100%{transform:translateX(1020px) skewX(-24deg)}}
    @keyframes errShake{0%,100%{transform:none}25%{transform:translateX(-3px)}50%{transform:translateX(3px)}75%{transform:translateX(-2px)}}
    @keyframes dispatchNudge{0%{transform:none}26%{transform:translateX(5.5px)}56%{transform:translateX(-1px)}100%{transform:none}}
    @keyframes dispatchGlow{0%,100%{opacity:0}22%{opacity:.65}56%{opacity:.15}}
    @keyframes failoverPulse{0%,100%{transform:none}32%{transform:translateX(-3px)}68%{transform:translateX(3px)}}
    @keyframes failoverNod{0%,100%{transform:none}40%{transform:translateX(2.5px) rotate(1deg)}70%{transform:translateX(-1px)}}
    @keyframes failoverSpark{0%{opacity:0;transform:scale(.2)}28%{opacity:1;transform:translate(15px,17px) scale(1)}100%{opacity:0;transform:translate(26px,31px) scale(.2)}}
    @media (prefers-reduced-motion:reduce){*{animation:none!important;transition:none!important}.${cls("s-top")},.${cls("s-bot")},.${cls("s-mid")},.${cls("r-body")},.${cls("r-leg")},.${cls("dg")},.${cls("t1")},.${cls("t2")},.${cls("dot")}{opacity:1!important;transform:none!important}.${cls("spk")},.${cls("tip-glow")},.${cls("pk-top")},.${cls("pk-mid")},.${cls("pk-bot")},.${cls("r-glint")},.${cls("wm-glint")}{opacity:0!important}}
  `;
  const css = reducedMotion ? staticCss : animatedCss;

  return (
    <svg
      id={id("logo")}
      xmlns="http://www.w3.org/2000/svg"
      viewBox={variant === "compact" ? "64 56 296 182" : "0 0 1000 300"}
      width="100%"
      height="100%"
      role={decorative ? undefined : "img"}
      aria-hidden={decorative || undefined}
      aria-label={decorative ? undefined : ariaLabel}
      focusable="false"
      data-state={state}
      data-brand-surface={surface}
      className={cx("block", className)}
    >
      <defs>
        <linearGradient id={id("rMainGrad")} x1="184" y1="68" x2="334" y2="226" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="var(--comp-brand-r-top)" />
          <stop offset="55%" stopColor="var(--comp-brand-r-mid)" />
          <stop offset="100%" stopColor="var(--comp-brand-r-bot)" />
        </linearGradient>
        <linearGradient id={id("rBevelGrad")} x1="184" y1="68" x2="352" y2="226" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#E5ECF8" />
          <stop offset="100%" stopColor="#AFBFE1" />
        </linearGradient>
        <linearGradient id={id("rLegGrad")} x1="270" y1="175" x2="328" y2="226" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#8E9EC6" />
          <stop offset="60%" stopColor="#E5ECF9" />
          <stop offset="100%" stopColor="#FFFFFF" />
        </linearGradient>
        <linearGradient id={id("sTopGrad")} x1="90" y1="0" x2="272" y2="0" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#040712" />
          <stop offset="24%" stopColor="#006EE8" />
          <stop offset="100%" stopColor="#00E1FF" />
        </linearGradient>
        <linearGradient id={id("sMidGrad")} x1="80" y1="0" x2="316" y2="0" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#040712" />
          <stop offset="22%" stopColor="#0B47CC" />
          <stop offset="100%" stopColor="#00C0FF" />
        </linearGradient>
        <linearGradient id={id("sBotGrad")} x1="70" y1="0" x2="208" y2="0" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#060412" />
          <stop offset="30%" stopColor="#4B1CC9" />
          <stop offset="100%" stopColor="#8B3FF9" />
        </linearGradient>
        <linearGradient id={id("wmGrad")} x1="404" y1="94" x2="880" y2="175" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="var(--comp-brand-wm-top)" />
          <stop offset="100%" stopColor="var(--comp-brand-wm-bot)" />
        </linearGradient>
        <linearGradient id={id("tagRGrad")} x1="610" y1="0" x2="882" y2="0" gradientUnits="userSpaceOnUse">
          <stop offset="0%" stopColor="#00A6FF" />
          <stop offset="50%" stopColor="#2D6BFF" />
          <stop offset="100%" stopColor="#8E3DFF" />
        </linearGradient>
        <linearGradient id={id("photonGrad")} x1="0%" y1="0%" x2="100%" y2="0%">
          <stop offset="0%" stopColor="#FFF" stopOpacity="0" />
          <stop offset="85%" stopColor="#FFF" stopOpacity=".95" />
          <stop offset="100%" stopColor="#FFF" stopOpacity="0" />
        </linearGradient>
        <linearGradient id={id("glintGrad")} x1="0%" y1="0%" x2="100%" y2="20%">
          <stop offset="0%" stopColor="#FFF" stopOpacity="0" />
          <stop offset="50%" stopColor="#FFF" stopOpacity=".9" />
          <stop offset="100%" stopColor="#FFF" stopOpacity="0" />
        </linearGradient>
        <filter id={id("lift")} x="-12%" y="-15%" width="124%" height="135%">
          <feDropShadow dx="0" dy="2" stdDeviation="3" floodColor="#22335C" floodOpacity=".14" />
        </filter>
        <filter id={id("bloom")} x="-45%" y="-65%" width="190%" height="230%">
          <feGaussianBlur stdDeviation="4" result="b" />
          <feMerge>
            <feMergeNode in="b" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
        <clipPath id={id("cR")}>
          <path d="M184 68h118a54 54 0 0 1 0 108h-67l-50 50h-37l76-76h78a28 28 0 0 0 0-56h-92Z" />
        </clipPath>
        <clipPath id={id("cTop")}>
          <path d="M96.5 102H272v13H96.5a6.5 6.5 0 0 1 0-13Z" />
        </clipPath>
        <clipPath id={id("cMid")}>
          <path d="M86.5 122H284l-7-13.5 39 20-39 20 7-13.5H86.5a6.5 6.5 0 0 1 0-13Z" />
        </clipPath>
        <clipPath id={id("cBot")}>
          <path d="M76.5 142H208l-13 13H76.5a6.5 6.5 0 0 1 0-13Z" />
        </clipPath>
        <clipPath id={id("cWm")}>
          <path fillRule="evenodd" d="M404 94h43a24 24 0 0 1 7.8 46.7L478 174h-15.6l-21.2-31.8h-24V174H404Zm13.2 12.5h29a11.8 11.8 0 0 1 0 23.5h-29Z" />
          <path fillRule="evenodd" d="M493 94h43a24 24 0 0 1 7.8 46.7L567 174h-15.6l-21.2-31.8h-24V174H493Zm13.2 12.5h29a11.8 11.8 0 0 1 0 23.5h-29Z" />
          <path fillRule="evenodd" d="M608 115.5a30.5 29.5 0 1 1-.1 0Zm0 12.5a17.8 17.2 0 1 0 .1 0Z" />
          <path d="M654 117h13v31.5a14.5 14.5 0 0 0 29 0V117h13v57h-13v-7.2c-4.5 5.5-10.6 8.2-17.8 8.2-14.7 0-24.2-10.8-24.2-26.5Z" />
          <path d="M734 100.5h13V117h13.5v11.5H747v27c0 5.5 3 7.5 9 7.5h5.5v11.5h-8.7c-13.3 0-18.8-6.7-18.8-18.5v-27.5h-11.5V117H734Z" />
          <path fillRule="evenodd" d="M826.5 160.5c-5.1 9.3-14.3 14.5-25.5 14.5-17.2 0-30-12.8-30-29.5s12.8-29.5 29.5-29.5 29.5 12.2 29.5 29v5.5h-46c1.4 8.7 8.2 13.7 17.8 13.7 6.6 0 11.6-2.6 15.2-7.2ZM784 140.5h33.2c-1.6-9.1-8.2-13.7-16.7-13.7s-15 4.6-16.5 13.7Z" />
          <path d="M846 117h12.5v8.8c3.5-6.2 9.1-8.8 17.3-8.8h6.7v12H874c-10.2 0-15.2 5.6-15.2 16.2V174H846Z" />
        </clipPath>
        <style>{css}</style>
      </defs>

      <g id={id("emblem")}>
        <g className={cls("r-react")}>
          <g className={cls("r-leg")} filter={`url(#${id("lift")})`}>
            <path d="M252 176h37l50 50h-37Z" fill={`url(#${id("rLegGrad")})`} stroke={`url(#${id("rBevelGrad")})`} strokeWidth=".9" strokeLinejoin="round" />
          </g>
          <g className={cls("r-body")} filter={`url(#${id("lift")})`}>
            <path d="M184 68h118a54 54 0 0 1 0 108h-67l-50 50h-37l76-76h78a28 28 0 0 0 0-56h-92Z" fill={`url(#${id("rMainGrad")})`} stroke={`url(#${id("rBevelGrad")})`} strokeWidth=".95" strokeLinejoin="round" />
            <g clipPath={`url(#${id("cR")})`}>
              <rect className={cls("r-glint")} x="0" y="60" width="105" height="175" fill={`url(#${id("glintGrad")})`} />
            </g>
          </g>
          <path
            id={id("switch-arc")}
            d="M306 104.5a24 24 0 0 1 0 48"
            fill="none"
            stroke={arc}
            strokeWidth="2.4"
            strokeLinecap="round"
            opacity={state === "idle" ? 0 : 0.95}
            style={{
              transformOrigin: "306px 128.5px",
              transform: `rotate(${angleDeg}deg)`,
              transition: reducedMotion
                ? "none"
                : "transform .28s cubic-bezier(.22,1,.36,1),opacity .2s,stroke .2s",
            }}
          />
          <g pointerEvents="none">
            <circle className={cx(cls("spk"), cls("spk-a"))} cx="315" cy="113" r="3.2" fill="#00D8FF" />
            <circle className={cx(cls("spk"), cls("spk-b"))} cx="321" cy="128.5" r="3.8" fill="#0088FF" />
            <circle className={cx(cls("spk"), cls("spk-c"))} cx="315" cy="144" r="3.2" fill="#8B3FF9" />
            <circle className={cx(cls("spk"), cls("spk-d"))} cx="315" cy="144" r="3.6" fill="#10B981" />
          </g>
        </g>

        <g className={cls("s-top")}>
          <path d="M96.5 102H272v13H96.5a6.5 6.5 0 0 1 0-13Z" fill={`url(#${id("sTopGrad")})`} />
          <g clipPath={`url(#${id("cTop")})`}>
            <rect className={cls("pk-top")} x="0" y="100" width="84" height="17" fill={`url(#${id("photonGrad")})`} />
          </g>
        </g>
        <g className={cls("s-bot")}>
          <path d="M76.5 142H208l-13 13H76.5a6.5 6.5 0 0 1 0-13Z" fill={`url(#${id("sBotGrad")})`} />
          <g clipPath={`url(#${id("cBot")})`}>
            <rect className={cls("pk-bot")} x="0" y="140" width="76" height="17" fill={`url(#${id("photonGrad")})`} />
          </g>
        </g>
        <g className={cls("s-mid")}>
          <g className={cls("a-nudge")}>
            <polygon className={cls("tip-glow")} points="277,109 316,128.5 277,148" fill="#00B8FF" filter={`url(#${id("bloom")})`} />
            <path d="M86.5 122H284l-7-13.5 39 20-39 20 7-13.5H86.5a6.5 6.5 0 0 1 0-13Z" fill={`url(#${id("sMidGrad")})`} />
            <g clipPath={`url(#${id("cMid")})`}>
              <rect className={cls("pk-mid")} x="0" y="106" width="96" height="46" fill={`url(#${id("photonGrad")})`} />
            </g>
          </g>
        </g>
      </g>

      <g id={id("wordmark")} fill={`url(#${id("wmGrad")})`} stroke="#C8D4EC" strokeWidth=".9" strokeLinejoin="round" filter={`url(#${id("lift")})`}>
        <path className={cx(cls("dg"), cls("dg1"))} fillRule="evenodd" d="M404 94h43a24 24 0 0 1 7.8 46.7L478 174h-15.6l-21.2-31.8h-24V174H404Zm13.2 12.5h29a11.8 11.8 0 0 1 0 23.5h-29Z" />
        <path className={cx(cls("dg"), cls("dg2"))} fillRule="evenodd" d="M493 94h43a24 24 0 0 1 7.8 46.7L567 174h-15.6l-21.2-31.8h-24V174H493Zm13.2 12.5h29a11.8 11.8 0 0 1 0 23.5h-29Z" />
        <path className={cx(cls("dg"), cls("dg3"))} fillRule="evenodd" d="M608 115.5a30.5 29.5 0 1 1-.1 0Zm0 12.5a17.8 17.2 0 1 0 .1 0Z" />
        <path className={cx(cls("dg"), cls("dg4"))} d="M654 117h13v31.5a14.5 14.5 0 0 0 29 0V117h13v57h-13v-7.2c-4.5 5.5-10.6 8.2-17.8 8.2-14.7 0-24.2-10.8-24.2-26.5Z" />
        <path className={cx(cls("dg"), cls("dg5"))} d="M734 100.5h13V117h13.5v11.5H747v27c0 5.5 3 7.5 9 7.5h5.5v11.5h-8.7c-13.3 0-18.8-6.7-18.8-18.5v-27.5h-11.5V117H734Z" />
        <path className={cx(cls("dg"), cls("dg6"))} fillRule="evenodd" d="M826.5 160.5c-5.1 9.3-14.3 14.5-25.5 14.5-17.2 0-30-12.8-30-29.5s12.8-29.5 29.5-29.5 29.5 12.2 29.5 29v5.5h-46c1.4 8.7 8.2 13.7 17.8 13.7 6.6 0 11.6-2.6 15.2-7.2ZM784 140.5h33.2c-1.6-9.1-8.2-13.7-16.7-13.7s-15 4.6-16.5 13.7Z" />
        <path className={cx(cls("dg"), cls("dg7"))} d="M846 117h12.5v8.8c3.5-6.2 9.1-8.8 17.3-8.8h6.7v12H874c-10.2 0-15.2 5.6-15.2 16.2V174H846Z" />
      </g>
      <g clipPath={`url(#${id("cWm")})`} pointerEvents="none">
        <rect className={cls("wm-glint")} x="0" y="88" width="130" height="96" fill={`url(#${id("glintGrad")})`} />
      </g>

      <g fontFamily="'Plus Jakarta Sans','Inter',system-ui,sans-serif" fontSize="29.5" letterSpacing=".35">
        <text className={cls("t1")} x="404" y="215" fontWeight="500" fill="var(--comp-brand-tag-muted)">One endpoint.</text>
        <text className={cls("t2")} x="610" y="215" fontWeight="600" fill={`url(#${id("tagRGrad")})`}>Smarter routing</text>
        <circle className={cls("dot")} cx="852" cy="212.5" r="2.7" fill="#8E3DFF" />
      </g>
    </svg>
  );
}
