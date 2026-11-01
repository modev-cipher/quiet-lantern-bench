import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";

export type ButtonVariant = "quiet" | "soft" | "solid" | "danger";
export type ButtonSize = "sm" | "md" | "lg";

type IconOnly = { icon: true; label: string };
type Labelled = { icon?: false; label?: undefined };

export type ButtonProps = Omit<
  ButtonHTMLAttributes<HTMLButtonElement>,
  "aria-label"
> & {
  variant?: ButtonVariant;
  size?: ButtonSize;
  block?: boolean;
  busy?: boolean;
  leading?: ReactNode;
  trailing?: ReactNode;
} & (IconOnly | Labelled);

const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  {
    variant = "quiet",
    size = "md",
    block = false,
    busy = false,
    icon = false,
    label,
    leading,
    trailing,
    type = "button",
    disabled,
    title,
    className,
    children,
    ...rest
  },
  ref,
) {
  return (
    <button
      ref={ref}
      type={type}
      data-variant={variant}
      data-size={size}
      data-icon={icon ? "true" : undefined}
      data-block={block ? "true" : undefined}
      aria-label={icon ? label : undefined}
      aria-busy={busy ? true : undefined}
      title={title ?? (icon ? label : undefined)}
      disabled={disabled === true || busy}
      className={className == null ? "ui-btn" : `ui-btn ${className}`}
      {...rest}
    >
      {busy ? <span aria-hidden="true" className="ui-spin" /> : leading}
      {children}
      {trailing}
    </button>
  );
});

export default Button;
