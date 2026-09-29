module PostgresBlog
  class Application < Rails::Application
    config.active_record.schema_format = :sql
  end
end
