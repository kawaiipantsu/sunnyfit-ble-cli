use serde::Deserialize;
static FOODS: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
pub fn init(path: &std::path::Path) -> anyhow::Result<()> {
    let catalogue: Catalogue = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| anyhow::anyhow!("invalid food catalogue JSON"))?;
    if catalogue.foods.is_empty()
        || catalogue.foods.iter().any(|f| {
            !f.kcal.is_finite() || f.kcal <= 0. || f.name.is_empty() || f.portion.is_empty()
        })
    {
        anyhow::bail!("foods require names, portions and positive calories");
    }
    let _ = FOODS.set(catalogue);
    Ok(())
}
#[derive(Deserialize)]
struct Catalogue {
    foods: Vec<Food>,
}
#[derive(Deserialize)]
struct Food {
    name: String,
    portion: String,
    kcal: f64,
}
/// Keep comparisons near one serving and never imply measured food intake.
pub fn message(calories: Option<f64>) -> String {
    let Some(kcal) = calories.filter(|v| v.is_finite() && *v > 0.) else {
        return "A little movement goes a long way.".into();
    };
    let catalogue = FOODS.get_or_init(|| {
        serde_json::from_str(include_str!("../callories-burn.json"))
            .expect("bundled food catalogue")
    });
    let Some(food) = catalogue
        .foods
        .iter()
        .filter(|f| f.kcal.is_finite() && f.kcal > 0.)
        .min_by(|a, b| {
            (kcal / a.kcal - 1.)
                .abs()
                .total_cmp(&(kcal / b.kcal - 1.).abs())
        })
    else {
        return String::new();
    };
    let ratio = kcal / food.kcal;
    if ratio < 0.5 {
        format!(
            "Every turn counts! About {:.0}% of {} so far.",
            ratio * 100.,
            food.name
        )
    } else if ratio < 1. {
        format!(
            "Wow, that's almost {}! (~{:.0} kcal per {})",
            food.name, food.kcal, food.portion
        )
    } else if ratio < 1.25 {
        format!(
            "That's about {}! (~{:.0} kcal per {})",
            food.name, food.kcal, food.portion
        )
    } else {
        format!(
            "About {:.1} portions of {} worth of energy! (approx.)",
            ratio, food.name
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn food_messages_require_energy() {
        assert!(!message(None).contains("pizza"));
        assert!(message(Some(280.)).contains("pizza"));
        assert!(message(Some(f64::NAN)).contains("movement"));
    }
}
