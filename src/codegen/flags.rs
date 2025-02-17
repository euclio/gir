use super::{function, trait_impls};
use crate::{
    analysis::flags::Info,
    analysis::special_functions::Type,
    codegen::general::{
        self, cfg_deprecated, derives, version_condition, version_condition_string,
    },
    config::gobjects::GObject,
    env::Env,
    file_saver,
    library::*,
    nameutil::{bitfield_member_name, use_glib_type},
    traits::*,
};
use std::{
    io::{prelude::*, Result},
    path::Path,
};

pub fn generate(env: &Env, root_path: &Path, mod_rs: &mut Vec<String>) {
    if env.analysis.flags.is_empty() {
        return;
    }

    let path = root_path.join("flags.rs");
    file_saver::save_to_file(path, env.config.make_backup, |w| {
        general::start_comments(w, &env.config)?;
        general::uses(w, env, &env.analysis.flags_imports, None)?;
        writeln!(w)?;

        mod_rs.push("\nmod flags;".into());
        for flags_analysis in &env.analysis.flags {
            let config = &env.config.objects[&flags_analysis.full_name];
            let flags = flags_analysis.type_(&env.library);

            if let Some(cfg) = version_condition_string(env, flags.version, false, 0) {
                mod_rs.push(cfg);
            }
            mod_rs.push(format!("pub use self::flags::{};", flags.name));
            generate_flags(env, w, flags, config, flags_analysis)?;
        }

        Ok(())
    });
}

#[allow(clippy::write_literal)]
fn generate_flags(
    env: &Env,
    w: &mut dyn Write,
    flags: &Bitfield,
    config: &GObject,
    analysis: &Info,
) -> Result<()> {
    let sys_crate_name = env.main_sys_crate_name();
    cfg_deprecated(w, env, flags.deprecated_version, false, 0)?;
    version_condition(w, env, flags.version, false, 0)?;
    writeln!(w, "bitflags! {{")?;
    if config.must_use {
        writeln!(w, "    #[must_use]")?;
    }

    if let Some(ref d) = config.derives {
        derives(w, &d, 1)?;
    }

    writeln!(w, "    pub struct {}: u32 {{", flags.name)?;
    for member in &flags.members {
        let member_config = config.members.matched(&member.name);
        let generate = member_config.iter().all(|m| m.status.need_generate());
        if !generate {
            continue;
        }

        let name = bitfield_member_name(&member.name);
        let val: i64 = member.value.parse().unwrap();
        let deprecated_version = member_config.iter().find_map(|m| m.deprecated_version);
        let version = member_config.iter().find_map(|m| m.version);
        cfg_deprecated(w, env, deprecated_version, false, 2)?;
        version_condition(w, env, version, false, 2)?;
        writeln!(w, "\t\tconst {} = {};", name, val as u32)?;
    }

    writeln!(
        w,
        "    }}
}}"
    )?;

    let functions = analysis
        .functions
        .iter()
        .filter(|f| f.status.need_generate())
        .collect::<Vec<_>>();

    if !functions.is_empty() {
        writeln!(w)?;
        version_condition(w, env, flags.version, false, 0)?;
        write!(w, "impl {} {{", analysis.name)?;
        for func_analysis in functions {
            function::generate(
                w,
                env,
                func_analysis,
                Some(&analysis.specials),
                flags.version,
                false,
                false,
                1,
            )?;
        }
        writeln!(w, "}}")?;
    }

    trait_impls::generate(
        w,
        env,
        &analysis.name,
        &analysis.functions,
        &analysis.specials,
        None,
        None,
    )?;

    writeln!(w)?;

    if config.generate_display_trait && !analysis.specials.has_trait(Type::Display) {
        // Generate Display trait implementation.
        version_condition(w, env, flags.version, false, 0)?;
        writeln!(
            w,
            "impl fmt::Display for {0} {{\n\
            \tfn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {{\n\
            \t\t<Self as fmt::Debug>::fmt(self, f)\n\
            \t}}\n\
            }}\n",
            flags.name
        )?;
    }

    version_condition(w, env, flags.version, false, 0)?;
    writeln!(
        w,
        "#[doc(hidden)]
impl IntoGlib for {name} {{
    type GlibType = {sys_crate_name}::{ffi_name};

    fn into_glib(self) -> {sys_crate_name}::{ffi_name} {{
        self.bits()
    }}
}}
",
        sys_crate_name = sys_crate_name,
        name = flags.name,
        ffi_name = flags.c_type
    )?;

    let assert = if env.config.generate_safety_asserts {
        "skip_assert_initialized!();\n\t\t"
    } else {
        ""
    };

    version_condition(w, env, flags.version, false, 0)?;
    writeln!(
        w,
        "#[doc(hidden)]
impl FromGlib<{sys_crate_name}::{ffi_name}> for {name} {{
    unsafe fn from_glib(value: {sys_crate_name}::{ffi_name}) -> Self {{
        {assert}Self::from_bits_truncate(value)
    }}
}}
",
        sys_crate_name = sys_crate_name,
        name = flags.name,
        ffi_name = flags.c_type,
        assert = assert
    )?;

    if let Some(ref get_type) = flags.glib_get_type {
        let configured_functions = config.functions.matched("get_type");
        let version = std::iter::once(flags.version)
            .chain(configured_functions.iter().map(|f| f.version))
            .max()
            .flatten();

        version_condition(w, env, version, false, 0)?;
        writeln!(
            w,
            "impl StaticType for {name} {{
    fn static_type() -> Type {{
        unsafe {{ from_glib({sys_crate_name}::{get_type}()) }}
    }}
}}",
            sys_crate_name = sys_crate_name,
            name = flags.name,
            get_type = get_type
        )?;
        writeln!(w)?;

        version_condition(w, env, version, false, 0)?;
        writeln!(
            w,
            "impl {valuetype} for {name} {{
    type Type = Self;
}}",
            name = flags.name,
            valuetype = use_glib_type(env, "value::ValueType"),
        )?;
        writeln!(w)?;

        version_condition(w, env, version, false, 0)?;
        writeln!(
            w,
            "unsafe impl<'a> FromValue<'a> for {name} {{
    type Checker = {genericwrongvaluetypechecker}<Self>;

    unsafe fn from_value(value: &'a {gvalue}) -> Self {{
        {assert}from_glib({glib}(value.to_glib_none().0))
    }}
}}",
            name = flags.name,
            glib = use_glib_type(env, "gobject_ffi::g_value_get_flags"),
            gvalue = use_glib_type(env, "Value"),
            genericwrongvaluetypechecker = use_glib_type(env, "value::GenericValueTypeChecker"),
            assert = assert,
        )?;
        writeln!(w)?;

        version_condition(w, env, version, false, 0)?;
        writeln!(
            w,
            "impl ToValue for {name} {{
    fn to_value(&self) -> {gvalue} {{
        let mut value = {gvalue}::for_value_type::<Self>();
        unsafe {{
            {glib}(value.to_glib_none_mut().0, self.into_glib());
        }}
        value
    }}

    fn value_type(&self) -> {gtype} {{
        Self::static_type()
    }}
}}",
            name = flags.name,
            glib = use_glib_type(env, "gobject_ffi::g_value_set_flags"),
            gvalue = use_glib_type(env, "Value"),
            gtype = use_glib_type(env, "Type"),
        )?;
        writeln!(w)?;
    }

    Ok(())
}
