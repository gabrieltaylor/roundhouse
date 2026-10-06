require "active_record"
require "json"

fixture, generated = ARGV
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Schema.verbose = false
load File.join(fixture, "db/schema.rb")
class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
Dir[File.join(fixture, "app/models/**/*.rb")].sort.each { |path| load path }
connection = ActiveRecord::Base.connection
connection.execute("INSERT INTO receipts (id, slug) VALUES (11, 'shared'), (12, 'second'), (13, 'empty')")
connection.execute("INSERT INTO refunds (id, slug) VALUES (11, 'shared')")
connection.execute("INSERT INTO accounts (id, slug) VALUES (51, 'account'), (52, 'empty')")
connection.execute("INSERT INTO case_tickets (id, account_id) VALUES (61, 51), (62, 51)")
connection.execute("INSERT INTO documents (id, code, position) VALUES (21, 'alpha', 2), (22, 'zeta', 1), (23, 'audit', 3), (24, 'hidden', 0)")
connection.execute(<<~SQL)
  INSERT INTO allocations (id, payable_slug, payer_kind, document_code, audit_code, position, active) VALUES
  (101, 'shared', 'Ledger::Payment', 'zeta', 'audit', 2, 1),
  (102, 'shared', 'Ledger::Payment', 'alpha', 'audit', 1, 1),
  (103, 'shared', 'Ledger::Refund', 'alpha', 'audit', 0, 1),
  (104, 'shared', 'Ledger::Payment', 'hidden', 'audit', 0, 0),
  (105, 'second', 'Ledger::Payment', 'alpha', 'audit', 1, 1)
SQL
connection.execute("UPDATE allocations SET document_type = 'Ledger::Invoice'")

associations = { "Ledger::Payment" => %i[entries invoices bills legacy_bills audits documents archived_documents first_entry], "Ledger::Refund" => %i[entries], "Ledger::Entry" => %i[invoice audit_invoice restricted_invoice], "Ledger::Invoice" => %i[payers], "Ledger::Account" => %i[tickets] }
expected_legacy_ids = Ledger::Payment.all.map(&:legacy_bill_ids)
key_for = ->(record) { record.respond_to?(:code) ? record.code : record.id }
observe = lambda do |record, name|
  result = record.public_send(name)
  result.nil? ? nil : (result.respond_to?(:to_ary) ? result.to_a.map(&key_for) : key_for.call(result))
end
assigned = Ledger::Entry.new
assigned.invoice = Ledger::Invoice.find_by!(code: "alpha")
assigned.payable = Ledger::Payment.first
expected_assignment = [assigned.document_code, assigned.payable_slug, assigned.payer_kind]
expected = {}
associations.each do |class_name, names|
  klass = Object.const_get(class_name)
  owners = klass.all.to_a
  names.each { |name| expected[[class_name, name]] = owners.map { |owner| observe.call(owner, name) } }
  ActiveRecord::Associations::Preloader.new(records: owners, associations: names).call
  names.each do |name|
    actual = owners.map { |owner| observe.call(owner, name) }
    raise "Rails lazy/preload discrepancy #{class_name}##{name}" unless actual == expected[[class_name, name]]
  end
end

Object.send(:remove_const, :RoundhouseRelation) if defined?(RoundhouseRelation)
runtime = File.read(File.expand_path("../../runtime/ruby/active_record/relation.rb", __dir__), encoding: "UTF-8")
runtime = runtime.sub("class Relation", "class RoundhouseRelation")
ActiveRecord.module_eval(runtime.sub(/\Amodule ActiveRecord\n/, "").sub(/end\s*\z/, ""))
adapter = Object.new
adapter.define_singleton_method(:select_rows) { |sql| connection.select_all(sql).to_a }
adapter.define_singleton_method(:escape_value) { |value| connection.quote(value) }
ActiveRecord.define_singleton_method(:adapter) { adapter }

module Db
  class << self
    attr_accessor :connection
    def prepare(sql) = [connection.select_all(sql).to_a.each, nil]
    def step?(statement)
      statement[1] = statement[0].next
      true
    rescue StopIteration
      false
    end
    def finalize(statement) = nil
    def escape_string(value) = connection.quote(value)
    def escape_int(value) = value.to_i.to_s
  end
end
Db.connection = connection
[Ledger::Payment, Ledger::Refund, Ledger::Entry, Ledger::Invoice, Ledger::ArchivedInvoice, Ledger::LegacyInvoice, Ledger::Account, Ledger::Ticket].each do |klass|
  klass.define_singleton_method(:from_stmt) { |statement| instantiate(statement[1]) }
  klass.define_singleton_method(:_columns_sql) { column_names.map { |c| "#{table_name}.#{c}" }.join(", ") }
  klass.define_singleton_method(:_hydrate_all) { |sql| connection.select_all(sql).to_a.map { |row| instantiate(row) } }
end

associations.each do |class_name, names|
  klass = Object.const_get(class_name)
  basename = class_name.split("::").last.downcase + ".rb"
  source = Dir[File.join(generated, "**", basename)].map { |path| File.read(path, encoding: "UTF-8") }.find { |text| text.include?("_preload_dispatch") }
  raise "missing generated source #{class_name}" unless source
  RubyVM::InstructionSequence.compile(source)
  methods = source.scan(/^    def (?:self\.)?[^\n]+\n.*?^    end\n/m)
  selected = methods.select do |body|
    body.match?(/^    def (?:self\.)?(?:_preload_|preload_associations|__.*_cache)/) || names.any? { |name| body.start_with?("    def #{name}\n") } ||
      (class_name == "Ledger::Entry" && body.match?(/^    def (?:invoice|payable)=/)) || body.start_with?("    def legacy_bill_ids\n")
  end
  selected.each do |body|
    klass.class_eval(body.gsub("ActiveRecord::Relation.new", "ActiveRecord::RoundhouseRelation.new"))
  end
  owners = klass.all.to_a
  owners.each { |owner| owner.attributes.each { |key, value| owner.instance_variable_set("@#{key}", value) } }
  names.each do |name|
    lazy = owners.map { |owner| observe.call(owner, name) }
    raise "generated lazy reader #{class_name}##{name}: #{lazy.inspect}" unless lazy == expected[[class_name, name]]
    klass.public_send("_preload_batch_#{name}", owners)
    actual = owners.map do |owner|
      cached = owner.instance_variable_get("@#{name}_cache")
      raise "cache not marked loaded" unless owner.instance_variable_get("@#{name}_loaded")
      cached.nil? ? nil : (cached.is_a?(Array) ? cached.map(&key_for) : key_for.call(cached))
    end
    raise "#{class_name}##{name}: expected #{expected[[class_name, name]].inspect}, got #{actual.inspect}" unless actual == expected[[class_name, name]]
    raise "preloaded reader disagrees #{class_name}##{name}" unless owners.map { |owner| observe.call(owner, name) } == expected[[class_name, name]]
    raise "empty collection failed" unless klass.public_send("_preload_batch_#{name}", []).empty?
  end
end
owners = Ledger::Payment.all.to_a
owners.each { |owner| owner.attributes.each { |key, value| owner.instance_variable_set("@#{key}", value) } }
raise "through ids ignored the source primary key" unless owners.map(&:legacy_bill_ids) == expected_legacy_ids
assigned = Ledger::Entry.new
assigned.invoice = Ledger::Invoice.find_by!(code: "alpha")
assigned.payable = Ledger::Payment.first
actual_assignment = %i[document_code payable_slug payer_kind].map { |key| assigned.instance_variable_get("@#{key}") }
raise "association writers lost declared keys: #{actual_assignment.inspect}" unless actual_assignment == expected_assignment
load File.join(generated, "static_preload.rb")
static = Ledger::Payment.static_batch.map { |owner| owner.instance_variable_get(:@entries_cache).map(&:id) }
raise "static preload differs: #{static.inspect}" unless static == expected[["Ledger::Payment", :entries]]
puts "Generated direct and through preloads match Rails, including scopes, order, empty owners and polymorphic collisions."
